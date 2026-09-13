"""Content-discarding process execution with isolated state and tree cleanup."""

from __future__ import annotations

import os
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO, Mapping, Sequence

DEFAULT_OUTPUT_LIMIT = 8 * 1024 * 1024
_CREDENTIAL_WORDS = (
    "API_KEY",
    "AUTH",
    "COOKIE",
    "CREDENTIAL",
    "PASSWORD",
    "SECRET",
    "TOKEN",
)


@dataclass(frozen=True)
class ProcessResult:
    """Metadata-only settlement for one child process."""

    returncode: int | None
    timed_out: bool
    output_exceeded: bool
    duration_ms: float
    first_output_ms: float | None
    stdout_bytes: int
    stderr_bytes: int
    settlement: str


class ProcessLaunchError(RuntimeError):
    """A process could not be created; the exception intentionally omits argv."""


def _mkdir_private(path: Path) -> None:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    if os.name != "nt":
        path.chmod(0o700)


def isolated_environment(
    root: Path,
    heycode_home: Path,
    workspace: Path,
    seed: int,
    *,
    pass_env: Sequence[str] = (),
    extra: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Build a minimal environment with private HOME/HEYCODE_HOME/workspace roots.

    Ambient credential-shaped names are absent unless their exact name appears
    in ``pass_env``. Callers never serialize returned values.
    """

    if not root.is_absolute() or not heycode_home.is_absolute() or not workspace.is_absolute():
        raise ValueError("isolation roots must be absolute")
    private_home = root / "home"
    temporary = root / "tmp"
    for directory in (root, private_home, temporary, heycode_home, workspace):
        _mkdir_private(directory)

    environment: dict[str, str] = {}
    for name in ("PATH", "SystemRoot", "COMSPEC", "PATHEXT"):
        value = os.environ.get(name)
        if value:
            environment[name] = value
    for name in pass_env:
        if name not in os.environ:
            raise ValueError("an explicitly inherited environment name is absent")
        environment[name] = os.environ[name]
    environment.update(
        {
            "HOME": str(private_home),
            "USERPROFILE": str(private_home),
            "XDG_CACHE_HOME": str(private_home / ".cache"),
            "XDG_CONFIG_HOME": str(private_home / ".config"),
            "XDG_DATA_HOME": str(private_home / ".local/share"),
            "TMPDIR": str(temporary),
            "TMP": str(temporary),
            "TEMP": str(temporary),
            "HEYCODE_HOME": str(heycode_home),
            "HEYCODE_EVAL_WORKSPACE": str(workspace),
            "HEYCODE_EVAL_SEED": str(seed),
            "TERM": "dumb",
            "NO_COLOR": "1",
            "RUST_BACKTRACE": "0",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
        }
    )
    if extra:
        for name, value in extra.items():
            if not name or "=" in name or "\x00" in name or "\x00" in value:
                raise ValueError("invalid environment entry")
            environment[name] = value
    inherited = set(pass_env)
    for name in tuple(environment):
        if name not in inherited and any(word in name.upper() for word in _CREDENTIAL_WORDS):
            if not extra or name not in extra:
                environment.pop(name)
    return environment


def _terminate_tree(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=0.4)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def run_discarded(
    argv: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str],
    timeout_s: float,
    stdin_bytes: bytes | None = None,
    output_limit: int = DEFAULT_OUTPUT_LIMIT,
) -> ProcessResult:
    """Run one process, retaining only timings, byte counts and closed outcomes."""

    if not argv or timeout_s <= 0 or output_limit <= 0:
        raise ValueError("invalid process specification")
    start = time.monotonic()
    creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    try:
        process = subprocess.Popen(
            list(argv),
            cwd=cwd,
            env=dict(env),
            stdin=subprocess.PIPE if stdin_bytes is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=os.name != "nt",
            creationflags=creation_flags,
        )
    except (OSError, ValueError) as error:
        raise ProcessLaunchError("process launch failed") from error

    lock = threading.Lock()
    first_output: list[float] = []
    counts = {"stdout": 0, "stderr": 0}
    output_exceeded = threading.Event()

    def drain(stream: BinaryIO, name: str) -> None:
        while True:
            chunk = stream.read(65536)
            if not chunk:
                return
            now = time.monotonic()
            with lock:
                if not first_output:
                    first_output.append(now)
                counts[name] += len(chunk)
                if counts["stdout"] + counts["stderr"] > output_limit:
                    output_exceeded.set()
                    return

    assert process.stdout is not None
    assert process.stderr is not None
    threads = [
        threading.Thread(target=drain, args=(process.stdout, "stdout"), daemon=True),
        threading.Thread(target=drain, args=(process.stderr, "stderr"), daemon=True),
    ]
    for thread in threads:
        thread.start()
    if stdin_bytes is not None:
        assert process.stdin is not None
        try:
            process.stdin.write(stdin_bytes)
            process.stdin.close()
        except BrokenPipeError:
            pass

    timed_out = False
    deadline = start + timeout_s
    while process.poll() is None:
        if output_exceeded.is_set():
            _terminate_tree(process)
            break
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            timed_out = True
            _terminate_tree(process)
            break
        time.sleep(min(0.02, remaining))
    try:
        process.wait(timeout=1.0)
    except subprocess.TimeoutExpired:
        _terminate_tree(process)
        process.wait(timeout=1.0)
    for thread in threads:
        thread.join(timeout=1.0)
    process.stdout.close()
    process.stderr.close()
    end = time.monotonic()
    settlement = "exited"
    if timed_out:
        settlement = "timeout_killed"
    elif output_exceeded.is_set():
        settlement = "output_killed"
    return ProcessResult(
        returncode=process.returncode,
        timed_out=timed_out,
        output_exceeded=output_exceeded.is_set(),
        duration_ms=round((end - start) * 1000.0, 6),
        first_output_ms=(
            round((first_output[0] - start) * 1000.0, 6) if first_output else None
        ),
        stdout_bytes=counts["stdout"],
        stderr_bytes=counts["stderr"],
        settlement=settlement,
    )
