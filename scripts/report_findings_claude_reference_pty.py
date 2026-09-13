#!/usr/bin/env python3
"""Capture Claude Code's ReportFindings UI against a localhost-only fixture.

The script runs the installed Claude CLI in a disposable config and git
workspace. It strips inherited Anthropic credentials, forces Messages API
traffic to a loopback server, invokes the built-in high-effort review command,
and returns one deterministic ReportFindings call. No commercial model or
external endpoint is used.
"""
from __future__ import annotations

import argparse
import fcntl
import http.server
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
import threading
import time
import uuid
from typing import Any

import pyte

from terminal_screenshot import render_screen


FINDING = {
    "file": "review.py",
    "line": 2,
    "summary": "Unchecked indexing can fail for short input.",
    "short_summary": "Unchecked indexing",
    "failure_scenario": (
        "Calling first([]) or first([value]) raises IndexError instead of "
        "returning a value."
    ),
    "category": "correctness",
    "verdict": "CONFIRMED",
}


class Screen(pyte.Screen):
    """Reset pyte when Claude enters its alternate screen."""

    def set_mode(self, *modes: int, **kwargs: Any) -> Any:
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []
    report_schema: dict[str, object] | None = None
    request_lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond_json(self, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            nonlocal report_schema
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            request = json.loads(raw)
            path = self.path
            tools = request.get("tools", [])
            names = [
                str(tool.get("name", ""))
                for tool in tools
                if isinstance(tool, dict)
            ]
            with request_lock:
                requests.append({"path": path, "tools": names})
            if "count_tokens" in path:
                self.respond_json({"input_tokens": 0})
                return

            matching = [
                tool
                for tool in tools
                if isinstance(tool, dict) and tool.get("name") == "ReportFindings"
            ]
            assert len(matching) == 1, names
            report_schema = matching[0]
            messages = request.get("messages", [])
            has_result = any(
                isinstance(block, dict)
                and block.get("type") == "tool_result"
                and block.get("tool_use_id") == "toolu_local_report"
                for message in messages
                if isinstance(message, dict)
                for block in (
                    message.get("content", [])
                    if isinstance(message.get("content"), list)
                    else []
                )
            )

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def event(kind: str, value: object) -> None:
                self.wfile.write(
                    f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode()
                )

            event(
                "message_start",
                {
                    "type": "message_start",
                    "message": {
                        "id": f"msg_local_{len(requests)}",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-opus-5",
                        "content": [],
                        "stop_reason": None,
                        "stop_sequence": None,
                        "usage": {
                            "input_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                            "output_tokens": 0,
                        },
                    },
                },
            )
            if has_result:
                event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""},
                    },
                )
                event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "text_delta",
                            "text": (
                                "review.py:2 — Unchecked indexing can fail for "
                                "short input."
                            ),
                        },
                    },
                )
                stop_reason = "end_turn"
            else:
                arguments = {"level": "high", "findings": [FINDING]}
                event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "tool_use",
                            "id": "toolu_local_report",
                            "name": "ReportFindings",
                            "input": {},
                        },
                    },
                )
                event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": json.dumps(arguments),
                        },
                    },
                )
                stop_reason = "tool_use"
            event("content_block_stop", {"type": "content_block_stop", "index": 0})
            event(
                "message_delta",
                {
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": stop_reason,
                        "stop_sequence": None,
                    },
                    "usage": {"output_tokens": 0},
                },
            )
            event("message_stop", {"type": "message_stop"})
            self.wfile.flush()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    session_id = str(uuid.uuid4())
    transcript = bytearray()
    captures: list[str] = []
    process: subprocess.Popen[bytes] | None = None
    master: int | None = None
    result: dict[str, object] = {
        "status": "failed",
        "scope": "installed Claude Code ReportFindings UI with localhost API fixture",
        "external_provider": False,
        "session_id": session_id,
    }

    try:
        with tempfile.TemporaryDirectory(prefix="claude-report-findings-") as folder:
            root = Path(folder)
            sandbox_home = root / "home"
            config = root / "config"
            workspace = root / "work"
            for path in (sandbox_home, config, workspace):
                path.mkdir()
            (workspace / "review.py").write_text(
                "def first(items):\n    return items[0]\n"
            )
            subprocess.run(["git", "init", "-q"], cwd=workspace, check=True)
            subprocess.run(["git", "add", "review.py"], cwd=workspace, check=True)
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=Local Fixture",
                    "-c",
                    "user.email=fixture@invalid",
                    "commit",
                    "-qm",
                    "base",
                ],
                cwd=workspace,
                check=True,
            )
            (workspace / "review.py").write_text(
                "def first(items):\n    return items[1]\n"
            )
            (config / ".claude.json").write_text(
                json.dumps(
                    {
                        "hasCompletedOnboarding": True,
                        "theme": "dark",
                        "lastOnboardingVersion": "2.1.268",
                    }
                )
            )
            environment = os.environ.copy()
            for key in list(environment):
                if key.startswith(
                    ("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")
                ):
                    environment.pop(key)
            environment.update(
                {
                    "HOME": str(sandbox_home),
                    "TERM": "xterm-256color",
                    "COLORTERM": "truecolor",
                    "CLAUDE_CONFIG_DIR": str(config),
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                    "CLAUDE_CODE_NO_FLICKER": "1",
                    "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                }
            )
            command = [
                executable,
                "--safe-mode",
                "--strict-mcp-config",
                "--no-chrome",
                "--setting-sources",
                "project,local",
                "--settings",
                '{"remoteControlAtStartup":false}',
                "--permission-mode",
                "manual",
                "--tools",
                "ReportFindings",
                "--session-id",
                session_id,
                "--name",
                "isolated-report-findings-reference",
                "--model",
                "opus",
            ]
            master, slave = pty.openpty()
            fcntl.ioctl(
                slave,
                termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, columns, 0, 0),
            )
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
                assert master is not None
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    ready, _, _ = select.select(
                        [master], [], [], min(0.05, deadline - time.monotonic())
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
                assert master is not None
                os.write(master, data)
                read(0.15)

            def wait_for(*needles: str, seconds: float | None = None) -> str:
                assert process is not None
                deadline = time.monotonic() + (seconds or timeout)
                current = ""
                while time.monotonic() < deadline:
                    current = read()
                    if any(needle in current for needle in needles):
                        return current
                    if process.poll() is not None:
                        raise RuntimeError(
                            f"Claude exited with {process.returncode} while waiting "
                            f"for {needles}:\n{current}"
                        )
                raise TimeoutError(f"Timed out waiting for {needles}:\n{current}")

            def capture(name: str) -> str:
                current = read(0.35)
                (output / f"{name}.txt").write_text(current)
                render_screen(screen, output / f"{name}.png")
                captures.append(name)
                return current

            startup = ""
            for _ in range(3):
                startup = wait_for(
                    "custom API key",
                    "trust this folder",
                    "for shortcuts",
                    "shift+tab",
                    "Try ",
                    seconds=20,
                )
                if "custom API key" in startup:
                    capture("00-local-key-confirmation")
                    send(b"\x1b[A\r")
                    continue
                if "trust this folder" in startup.lower():
                    send(b"\x1b[B\r")
                    continue
                break
            if not any(
                marker in startup.lower()
                for marker in ("for shortcuts", "shift+tab", "try ")
            ):
                startup = wait_for(
                    "for shortcuts", "shift+tab", "Try ", seconds=20
                )
            capture("01-ready")
            send(
                b"This is a controlled code review. Report the supplied finding "
                b"with ReportFindings: review.py line 2 has unchecked indexing, "
                b"which raises IndexError on short input."
            )
            read(0.5)
            send(b"\r")
            completed = wait_for("Unchecked indexing", "review.py:2", seconds=timeout)
            time.sleep(0.6)
            completed = capture("02-report-completed")
            assert "Unchecked indexing" in completed or "review.py:2" in completed
            assert report_schema is not None
            schema = report_schema.get("input_schema", {})
            assert isinstance(schema, dict)
            properties = schema.get("properties", {})
            assert isinstance(properties, dict) and "findings" in properties
            assert sum(
                1
                for request in requests
                if request["path"] == "/v1/messages?beta=true"
            ) == 2
            result.update(
                {
                    "status": "captured",
                    "claude_version": subprocess.run(
                        [executable, "--version"],
                        capture_output=True,
                        text=True,
                        check=True,
                    ).stdout.strip(),
                    "localhost_requests": len(requests),
                    "message_requests": sum(
                        1
                        for request in requests
                        if request["path"] == "/v1/messages?beta=true"
                    ),
                    "report_findings_present": True,
                    "report_findings_max_items": properties["findings"].get(
                        "maxItems"
                    ),
                    "captures": captures,
                }
            )
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
    finally:
        (output / "terminal.ansi").write_bytes(transcript)
        if process is not None and process.poll() is None:
            try:
                process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
        if process is not None:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
        if master is not None:
            os.close(master)
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        (output / "requests-summary.json").write_text(
            json.dumps(requests, indent=2, ensure_ascii=False)
        )
        if report_schema is not None:
            (output / "report-findings-schema.json").write_text(
                json.dumps(report_schema, indent=2, ensure_ascii=False)
            )
        result["captures"] = captures
        (output / "result.json").write_text(
            json.dumps(result, indent=2, ensure_ascii=False)
        )
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tmp/terminal-evidence/report-findings-claude-v1"),
    )
    parser.add_argument("--columns", type=int, default=120)
    parser.add_argument("--rows", type=int, default=52)
    parser.add_argument("--timeout", type=float, default=60)
    arguments = parser.parse_args()
    print(
        json.dumps(
            run(
                arguments.output,
                columns=arguments.columns,
                rows=arguments.rows,
                timeout=arguments.timeout,
            ),
            indent=2,
            ensure_ascii=False,
        )
    )
