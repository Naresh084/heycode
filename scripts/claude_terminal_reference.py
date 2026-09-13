#!/usr/bin/env python3
"""Capture an isolated live Claude terminal reference without touching user sessions.

The script starts a new UUID-scoped Claude session in a disposable directory,
asks for two read-only background agents and one background shell, and records
the actual terminal cells. It inherits the caller's Claude authentication but
does not reuse, attach to, or send input to any existing terminal session.
"""
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
    with tempfile.TemporaryDirectory(prefix="heycode-claude-reference-") as folder:
        workspace = Path(folder)
        (workspace / "alpha.txt").write_text("ALPHA_REFERENCE_CONTENT\n")
        (workspace / "beta.txt").write_text("BETA_REFERENCE_CONTENT\n")
        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
        environment = os.environ.copy()
        environment.update({"TERM": "xterm-256color", "COLORTERM": "truecolor"})
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
            "terminal-terminal-reference",
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

        def capture(name: str) -> str:
            visible = read(0.35)
            (output / f"{name}.txt").write_text(visible)
            (output / f"{name}.ansi").write_bytes(bytes(transcript))
            render_screen(screen, output / f"{name}.png")
            captures.append(name)
            return visible

        prompt = (
            "This is an isolated read-only terminal UI reference in a disposable directory. "
            "Do not edit, create, rename, or delete files. Start exactly two background agents "
            "in parallel, named ref-alpha and ref-beta. Each agent must first run Bash sleep 8, "
            "then read only alpha.txt or beta.txt respectively, report its file content, and stay "
            "available for follow-up. Also start one background Bash shell that prints REF_TICK_1 "
            "through REF_TICK_45 one per second. Briefly state when the agents and shell have been "
            "launched, then wait for their results."
        )
        blocker: str | None = None
        try:
            initial = read(3)
            if "trust" in initial.lower() and "folder" in initial.lower():
                # The stock dialog defaults to "No, exit". Move once to the
                # affirmative option and confirm this disposable directory.
                send(b"\x1b[B\r")
                initial = read(2)
            if "Claude Code" not in initial and "Welcome" not in initial:
                wait_for("Claude Code", "Welcome", seconds=20)
            wait_for("shift+tab to cycle", seconds=20)
            send(prompt.encode())
            read(0.5)
            send(b"\r")

            # Wait for the parent model turn to finish while the independent
            # shell is still alive. Slash commands typed during an active turn
            # are queued as ordinary messages by the current Claude client.
            wait_for("↓ to manage", seconds=timeout)
            capture("01-main-agent-switcher")
            # Preserve the settled two-agent result separately. Current Claude
            # can collapse the footer back to a compact count after settlement,
            # so this is evidence of completion, not a claimed two-row footer.
            wait_for(
                "Both agents finished",
                "Both agents have reported",
                "Both agents are now idle",
                seconds=timeout,
            )
            capture("02-background-results")
            wait_for(
                "All three background jobs are done",
                "All three background jobs are complete",
                seconds=timeout,
            )

            # /tasks is the public route to background work. Current Claude
            # builds may resolve directly to the selected agent inspector;
            # preserve that current behavior as evidence instead of assuming
            # Left must reveal a grouped list.
            send(b"/tasks\r")
            surface = wait_for("↑/↓ to select", "Prompt", "Progress", seconds=45)
            if "Prompt" in surface or "Progress" in surface:
                capture("03-agent-inspector")
                send(b"f")
            else:
                capture("02-background-list")
                # Stock order is team lead, agents, then shells.
                send(b"\x1b[B")
                read(0.5)
                send(b"\r")
                wait_for("Prompt", "Progress", seconds=20)
                capture("03-agent-inspector")
                send(b"f")
            wait_for("Message @", seconds=20)
            send(b"CLAUDE_REFERENCE_DRAFT")
            capture("04-agent-foreground-draft")
        except Exception as error:  # Preserve exact visual and textual blocker.
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
        "background_list_captured": "02-background-list" in captures,
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
