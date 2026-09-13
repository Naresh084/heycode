#!/usr/bin/env python3
"""Capture Claude's prompt-free schedule command availability in isolation.

The process uses a disposable HOME, CLAUDE_CONFIG_DIR, workspace, session ID,
and dummy API key. Bare/safe mode prevents keychain, plugin, memory, MCP, and
project-config reuse. The script types command prefixes but never presses Enter,
so it cannot create/list a hosted schedule or start a paid model turn.
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
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
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    transcript = bytearray()
    captures: list[str] = []
    blocker: str | None = None
    availability = "unknown"
    schedule_command_available: bool | None = None
    cron_command_available: bool | None = None
    remote_policy_boundary = False

    with tempfile.TemporaryDirectory(prefix="heycode-claude-schedule-reference-") as folder:
        root = Path(folder)
        home = root / "home"
        config = root / "claude-config"
        workspace = root / "workspace"
        home.mkdir()
        config.mkdir()
        workspace.mkdir()
        session_id = str(uuid.uuid4())

        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
        environment = os.environ.copy()
        for name in list(environment):
            if name.endswith("_API_KEY") or name.endswith("_AUTH_TOKEN"):
                environment.pop(name, None)
        environment.update(
            {
                "HOME": str(home),
                "CLAUDE_CONFIG_DIR": str(config),
                "ANTHROPIC_API_KEY": "sk-ant-heycode-schedule-reference-not-real",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
            }
        )
        command = [
            executable,
            "--bare",
            "--safe-mode",
            "--strict-mcp-config",
            "--no-chrome",
            "--permission-mode",
            "manual",
            "--session-id",
            session_id,
            "--name",
            "schedule-availability-reference",
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
                ready, _, _ = select.select(
                    [master], [], [], max(0, min(0.05, deadline - time.monotonic()))
                )
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

        def capture(name: str) -> str:
            visible = read(0.5)
            (output / f"{name}.txt").write_text(visible)
            render_screen(screen, output / f"{name}.png")
            captures.append(name)
            return visible

        try:
            initial = read(4)
            deadline = time.monotonic() + timeout
            while not any(
                marker in initial.lower()
                for marker in ("shift+tab to cycle", "manual mode on")
            ):
                lower = initial.lower()
                if "detected a custom api key" in lower and "want to use" in lower:
                    # The dummy key is intentionally selected only to reach the local
                    # command picker. No command or conversation prompt is submitted.
                    send(b"\x1b[A\r")
                elif "security notes:" in lower and "press enter to continue" in lower:
                    send(b"\r")
                elif "trust" in lower and "folder" in lower:
                    send(b"\x1b[B\r")
                elif "theme" in lower and "choose" in lower:
                    send(b"\r")
                elif any(
                    term in lower
                    for term in ("log in", "login", "authentication required")
                ):
                    availability = "unauthenticated"
                    break
                elif process.poll() is not None:
                    raise RuntimeError(
                        f"Claude exited with {process.returncode} during onboarding:\n"
                        f"{initial}"
                    )
                elif time.monotonic() >= deadline:
                    raise TimeoutError(f"timed out reaching the command picker:\n{initial}")
                initial = read(1)

            if availability == "unauthenticated":
                capture("00-unauthenticated-boundary")
            else:
                capture("00-ready-no-prompt")
                remote_policy_boundary = (
                    "remote managed settings failed to load" in initial.lower()
                    and "no remote policy applied" in initial.lower()
                )
                send(b"/schedule")
                schedule = capture("01-schedule-prefix")
                schedule_lines = [
                    line for line in schedule.splitlines() if "schedule" in line.lower()
                ]
                (output / "schedule-lines.txt").write_text(
                    "\n".join(schedule_lines) + "\n"
                )
                schedule_command_available = not any(
                    'no commands match "/schedule"' in line.lower()
                    for line in schedule_lines
                )
                send(b"\x1b")
                read(0.25)
                send(b"\x15")
                read(0.25)
                send(b"/cron")
                cron = capture("02-cron-prefix")
                cron_lines = [
                    line for line in cron.splitlines() if "cron" in line.lower()
                ]
                (output / "cron-lines.txt").write_text("\n".join(cron_lines) + "\n")
                cron_command_available = any(
                    line.strip().startswith("/cron ") or line.strip() == "/cron"
                    for line in cron.splitlines()
                )
                availability = "captured"
        except Exception as error:
            blocker = f"{type(error).__name__}: {error}"
            capture("blocked")
        finally:
            (output / "terminal.ansi").write_bytes(transcript)
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

        config_entries = [path for path in config.rglob("*") if path.is_file()]
        config_top_levels = sorted(
            {path.relative_to(config).parts[0] for path in config.iterdir()}
        )

    version = subprocess.run(
        [executable, "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    result: dict[str, object] = {
        "status": "blocked" if blocker else availability,
        "claude_version": version,
        "workspace": "disposable",
        "config": "disposable CLAUDE_CONFIG_DIR and HOME",
        "bare_mode": True,
        "dummy_api_key": True,
        "keychain_read": False,
        "remote_control_requested": False,
        "nonessential_traffic_disabled": True,
        "prompt_submitted": False,
        "command_executed": False,
        "hosted_schedule_created_or_listed": False,
        "built_in_command_picker": {
            "schedule_available": schedule_command_available,
            "cron_available": cron_command_available,
        },
        "remote_policy_boundary_observed": remote_policy_boundary,
        "captures": captures,
        "config_file_count": len(config_entries),
        "config_top_levels": config_top_levels,
        "blocker": blocker,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e"),
    )
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    parser.add_argument("--timeout", type=float, default=25)
    arguments = parser.parse_args()
    run(
        arguments.output,
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
