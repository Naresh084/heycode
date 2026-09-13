"""POSIX screen-reader first-frame probe that discards terminal content."""

from __future__ import annotations

import os
import pty
import select
import signal
import subprocess
import time
from pathlib import Path
from typing import Mapping, Sequence

from quality.process import ProcessLaunchError, ProcessResult


def _signal_tree_or_process(process: subprocess.Popen[bytes], requested: signal.Signals) -> None:
    """Signal the owned group, falling back to the exact child on host refusal."""

    try:
        os.killpg(process.pid, requested)
        return
    except (PermissionError, ProcessLookupError):
        pass
    if process.poll() is None:
        try:
            process.send_signal(requested)
        except (PermissionError, ProcessLookupError):
            pass


def _group_exists(group_id: int) -> bool:
    try:
        os.killpg(group_id, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def run_first_frame_probe(
    argv: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str],
    timeout_s: float,
) -> ProcessResult:
    """Measure first terminal bytes, request normal quit, and reap the group."""

    if os.name == "nt":
        raise ProcessLaunchError("terminal probe is unsupported on this platform")
    start = time.monotonic()
    master, slave = pty.openpty()
    try:
        try:
            process = subprocess.Popen(
                list(argv),
                cwd=cwd,
                env=dict(env),
                stdin=slave,
                stdout=slave,
                stderr=slave,
                start_new_session=True,
                close_fds=True,
            )
        except OSError as error:
            raise ProcessLaunchError("terminal process launch failed") from error
    finally:
        os.close(slave)

    first_output: float | None = None
    byte_count = 0
    deadline = start + timeout_s
    timed_out = False
    while process.poll() is None and time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], 0.05)
        if not readable:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        if first_output is None:
            first_output = time.monotonic()
        byte_count += len(chunk)
        if byte_count > 8 * 1024 * 1024:
            break
        if first_output is not None:
            os.write(master, b"\x03")
            time.sleep(0.05)
            os.write(master, b"\x03")
            break

    quit_deadline = min(deadline, time.monotonic() + 1.5)
    while process.poll() is None and time.monotonic() < quit_deadline:
        time.sleep(0.02)
    settlement = "exited"
    if process.poll() is None:
        timed_out = first_output is None and time.monotonic() >= deadline
        settlement = "timeout_killed" if timed_out else "probe_killed"
        _signal_tree_or_process(process, signal.SIGTERM)
        try:
            process.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            _signal_tree_or_process(process, signal.SIGKILL)
    try:
        process.wait(timeout=2.0)
    except subprocess.TimeoutExpired:
        settlement = "cleanup_failed"
    finally:
        os.close(master)
    if _group_exists(process.pid):
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (PermissionError, ProcessLookupError):
            pass
        time.sleep(0.05)
        if _group_exists(process.pid):
            settlement = "cleanup_failed"
    end = time.monotonic()
    return ProcessResult(
        returncode=process.returncode,
        timed_out=timed_out,
        output_exceeded=byte_count > 8 * 1024 * 1024,
        duration_ms=round((end - start) * 1000.0, 6),
        first_output_ms=(
            round((first_output - start) * 1000.0, 6) if first_output is not None else None
        ),
        stdout_bytes=byte_count,
        stderr_bytes=0,
        settlement=settlement,
    )
