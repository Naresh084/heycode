#!/usr/bin/env python3
"""Capture current Claude /focus and /color behavior in an isolated PTY."""
from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid
from pathlib import Path

import pyte

from terminal_screenshot import render_screen


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    output.mkdir(parents=True, exist_ok=True)
    session_id = str(uuid.uuid4())
    captures: list[str] = []
    transcript = bytearray()

    with tempfile.TemporaryDirectory(prefix="heycode-claude-focus-color-") as folder:
        workspace = Path(folder)
        (workspace / "reference.txt").write_text("REFERENCE_OLD\n")
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        environment = os.environ.copy()
        environment.update(
            {
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
                # /focus is intentionally fullscreen-only. Pin the reference
                # process to Claude's documented no-flicker renderer so a
                # previous renderer-start failure cannot downgrade this run.
                "CLAUDE_CODE_NO_FLICKER": "1",
            }
        )
        command = [
            executable,
            "--safe-mode",
            "--strict-mcp-config",
            "--no-chrome",
            "--model",
            "opus",
            "--effort",
            "high",
            "--permission-mode",
            "bypassPermissions",
            "--dangerously-skip-permissions",
            "--session-id",
            session_id,
            "--name",
            "terminal-focus-color-reference",
        ]
        process = subprocess.Popen(
            command,
            cwd=workspace,
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)
        screen = Screen(columns, rows)
        stream = pyte.ByteStream(screen)

        def read(seconds: float = 0.15) -> str:
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master], [], [], min(0.05, deadline - time.monotonic()))
                if not ready:
                    continue
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                transcript.extend(data)
                stream.feed(data)
            return "\n".join(screen.display)

        def send(data: bytes) -> None:
            os.write(master, data)

        def wait_for(*needles: str, seconds: float | None = None) -> str:
            deadline = time.monotonic() + (seconds or timeout)
            latest = ""
            while time.monotonic() < deadline:
                latest = read()
                if any(needle in latest for needle in needles):
                    return latest
                if process.poll() is not None:
                    raise RuntimeError(
                        f"Claude exited with {process.returncode} while waiting for {needles}:\n{latest}"
                    )
            raise TimeoutError(f"Timed out waiting for {needles}:\n{latest}")

        def wait_for_completed_turn(seconds: float | None = None) -> str:
            deadline = time.monotonic() + (seconds or timeout)
            latest = ""
            while time.monotonic() < deadline:
                latest = read()
                if (
                    latest.count("FOCUS_REFERENCE_COMPLETE") >= 2
                    and "esc to interrupt" not in latest
                ):
                    return latest
                if process.poll() is not None:
                    raise RuntimeError(
                        f"Claude exited with {process.returncode} before the turn settled:\n{latest}"
                    )
            raise TimeoutError(f"Timed out waiting for completed reference turn:\n{latest}")

        def capture(name: str) -> str:
            visible = read(0.35)
            (output / f"{name}.txt").write_text(visible)
            (output / f"{name}.ansi").write_bytes(bytes(transcript))
            render_screen(screen, output / f"{name}.png")
            captures.append(name)
            return visible

        blocker: str | None = None
        try:
            initial = read(3)
            if "trust" in initial.lower() and "folder" in initial.lower():
                send(b"\x1b[B\r")
                initial = read(2)
            if "Claude Code" not in initial and "Welcome" not in initial:
                wait_for("Claude Code", "Welcome", seconds=20)
            wait_for("shift+tab to cycle", seconds=20)

            prompt = (
                "Read reference.txt, replace REFERENCE_OLD with REFERENCE_NEW using the edit tool, "
                "then answer exactly FOCUS_REFERENCE_COMPLETE. Do not touch any other file."
            )
            send(prompt.encode())
            read(0.5)
            send(b"\r")
            wait_for_completed_turn()
            capture("01-normal-completed-turn")

            send(b"/focus\r")
            wait_for("Focus view enabled", seconds=30)
            capture("02-focus-view")

            send(b"/focus\r")
            wait_for("Focus view disabled", seconds=30)
            capture("03-normal-restored")

            send(b"/color\r")
            wait_for("Session color set to:", seconds=30)
            capture("04-color-no-argument")

            send(b"/color cyan\r")
            wait_for("Session color set to: cyan", seconds=30)
            capture("05-color-cyan")

            send(b"/color default\r")
            wait_for("Session color reset to default", seconds=30)
            capture("06-color-default")
        except Exception as error:
            blocker = f"{type(error).__name__}: {error}"
            capture("blocked")
        finally:
            if process.poll() is None:
                try:
                    process.send_signal(signal.SIGTERM)
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
            os.close(master)

    result: dict[str, object] = {
        "status": "blocked" if blocker else "passed",
        "claude_version": subprocess.run(
            [executable, "--version"], capture_output=True, text=True, check=False
        ).stdout.strip(),
        "session_id": session_id,
        "workspace": "disposable temporary directory",
        "captures": captures,
        "blocker": blocker,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=45)
    parser.add_argument("--timeout", type=float, default=120)
    arguments = parser.parse_args()
    print(
        json.dumps(
            run(
                arguments.output.resolve(),
                columns=arguments.columns,
                rows=arguments.rows,
                timeout=arguments.timeout,
            ),
            indent=2,
        )
    )
