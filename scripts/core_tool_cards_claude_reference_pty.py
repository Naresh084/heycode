#!/usr/bin/env python3
"""Capture Claude Code's core file/shell tool cards against a loopback fixture.

The installed Claude CLI runs in a disposable HOME/CLAUDE_CONFIG_DIR with every
inherited Anthropic credential stripped, `ANTHROPIC_API_KEY` set to a dummy
value and `ANTHROPIC_BASE_URL` pointed at a local HTTP server on 127.0.0.1.
That server streams deterministic `content_block` `tool_use` events for Read,
Write, Edit, Bash (fast, slow, failing, long output, denied), Glob and Grep, so
the transcript card, approval dialog, running, completed, grouped, expanded and
error states can be captured without any commercial provider call.
"""
from __future__ import annotations

import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
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

LONG_OUTPUT_LINES = 40

#: Deterministic tool calls the fixture streams, in order.  Each entry names
#: the Claude tool and builds its input from the disposable workspace root and
#: a sibling directory outside it (outside paths make Claude ask before
#: reading, which is the only way to reach the Read/Glob/Grep approval cards).
STEPS: list[dict[str, Any]] = [
    {
        "name": "Read",
        "input": lambda work, out: {"file_path": str(work / "sample.txt")},
    },
    {
        "name": "Read",
        "input": lambda work, out: {"file_path": str(out / "reference.txt")},
    },
    {
        "name": "Write",
        "input": lambda work, out: {
            "file_path": str(work / "notes.txt"),
            "content": "first note\nsecond note\nthird note\n",
        },
    },
    {
        "name": "Edit",
        "input": lambda work, out: {
            "file_path": str(work / "sample.txt"),
            "old_string": "beta",
            "new_string": "delta",
        },
    },
    {
        "name": "Bash",
        "running": True,
        "input": lambda work, out: {
            "command": "sleep 2; printf 'CORE_TOOLS_SLOW_DONE\\n'",
            "description": "Print the delayed core tool marker",
        },
    },
    {
        "name": "Glob",
        "input": lambda work, out: {"pattern": "*.txt", "path": str(out)},
    },
    {
        "name": "Grep",
        "input": lambda work, out: {
            "pattern": "delta",
            "path": str(out),
            "output_mode": "content",
        },
    },
    {
        "name": "Grep",
        "input": lambda work, out: {
            "pattern": "no-such-token-anywhere",
            "path": str(work),
            "output_mode": "content",
        },
    },
    {
        "name": "Bash",
        "input": lambda work, out: {
            "command": "printf 'core tools failure\\n' >&2; exit 3",
            "description": "Fail on purpose with exit code 3",
        },
    },
    {
        "name": "Bash",
        "input": lambda work, out: {
            "command": (
                "for i in $(seq 1 %d); do printf 'CORE_OUTPUT_LINE_%%s\\n' \"$i\"; done"
                % LONG_OUTPUT_LINES
            ),
            "description": "Print %d numbered output lines" % LONG_OUTPUT_LINES,
        },
    },
    {
        "name": "Bash",
        "input": lambda work, out: {
            "command": "printf 'must not run\\n' > denied.txt",
            "description": "Write a file the operator rejects",
        },
    },
    {
        "name": "Bash",
        "input": lambda work, out: {
            "command": "printf 'cancel me before running\\n' > cancelled.txt",
            "description": "A request the operator cancels with Escape",
        },
    },
]

#: Screen markers that decide how a pending approval is answered.
CANCEL_MARKER = "cancel me before running"
DENY_MARKER = "must not run"

FINAL_TEXT = "CORE_TOOL_CARDS_REFERENCE_DONE"

PROMPT = (
    "This is a controlled tool-UI capture. Follow the tool calls I stream back "
    "exactly and reply with the completion marker at the end."
)


def approval_slug(screen_text: str, index: int) -> str:
    """Name one pending approval from the dialog heading and its payload."""
    markers = [
        ("must not run", "bash-denied"),
        ("cancel me before running", "bash-cancelled"),
        ("sleep 2", "bash-slow"),
        ("exit 3", "bash-failing"),
        ("CORE_OUTPUT_LINE", "bash-long"),
        ("Create file", "write"),
        ("Edit file", "edit"),
        ("Read file", "read"),
        ("Glob", "glob"),
        ("Grep", "grep"),
        ("Search", "search"),
        ("Bash command", "bash"),
    ]
    for marker, slug in markers:
        if marker in screen_text:
            return slug
    return f"tool-{index}"


class Screen(pyte.Screen):
    """Reset pyte when Claude enters its alternate screen."""

    def set_mode(self, *modes: int, **kwargs: Any) -> Any:
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def _sha256(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def run(
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
    theme: str,
    color: bool,
    deny_action: str,
) -> dict[str, object]:
    executable = shutil.which("claude") or "/Users/naresh/.local/bin/claude"
    if not Path(executable).exists():
        raise RuntimeError("Claude CLI is not installed")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []
    request_lock = threading.Lock()
    workspace_holder: dict[str, Path] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond_json(self, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            request = json.loads(raw)
            names = [
                str(tool.get("name", ""))
                for tool in request.get("tools", [])
                if isinstance(tool, dict)
            ]
            messages = request.get("messages", [])
            results = sum(
                1
                for message in messages
                if isinstance(message, dict)
                for block in (
                    message.get("content", [])
                    if isinstance(message.get("content"), list)
                    else []
                )
                if isinstance(block, dict) and block.get("type") == "tool_result"
            )
            with request_lock:
                requests.append(
                    {"path": self.path, "tools": names, "tool_results_seen": results}
                )
            if "count_tokens" in self.path:
                self.respond_json({"input_tokens": 0})
                return

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
            if results < len(STEPS):
                step = STEPS[results]
                arguments = step["input"](
                    workspace_holder["work"], workspace_holder["outside"]
                )
                event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "tool_use",
                            "id": f"toolu_local_{results}",
                            "name": step["name"],
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
            else:
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
                        "delta": {"type": "text_delta", "text": FINAL_TEXT},
                    },
                )
                stop_reason = "end_turn"
            event("content_block_stop", {"type": "content_block_stop", "index": 0})
            event(
                "message_delta",
                {
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": None},
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
    observed: dict[str, object] = {}
    process: subprocess.Popen[bytes] | None = None
    master: int | None = None
    result: dict[str, object] = {
        "status": "failed",
        "engine": "claude-code",
        "scope": "core file and shell tool cards against a loopback Anthropic fixture",
        "external_provider": False,
        "external_endpoints": [],
        "theme": theme,
        "deny_action": deny_action,
        "color": color,
        "viewport": f"{columns}x{rows}",
        "session_id": session_id,
    }

    try:
        with tempfile.TemporaryDirectory(prefix="claude-core-tools-", dir="/tmp") as folder:
            root = Path(folder)
            sandbox_home = root / "home"
            config = root / "config"
            workspace = root / "work"
            outside = root / "outside"
            for path in (sandbox_home, config, workspace, outside):
                path.mkdir()
            workspace_holder["work"] = workspace
            workspace_holder["outside"] = outside
            (workspace / "sample.txt").write_text("alpha\nbeta\ngamma\n")
            (workspace / "other.txt").write_text("unrelated\n")
            (outside / "reference.txt").write_text("alpha\ndelta\ngamma\n")
            (outside / "second.txt").write_text("delta again\n")
            subprocess.run(["git", "init", "-q"], cwd=workspace, check=True)
            (config / ".claude.json").write_text(
                json.dumps(
                    {
                        "hasCompletedOnboarding": True,
                        "theme": theme,
        "deny_action": deny_action,
                        "lastOnboardingVersion": "2.1.269",
                    }
                )
            )
            environment = os.environ.copy()
            for key in list(environment):
                if key.startswith(("ANTHROPIC_", "CLAUDE_", "CLAUDECODE")):
                    environment.pop(key)
            environment.pop("NO_COLOR", None)
            environment.update(
                {
                    "HOME": str(sandbox_home),
                    "TERM": "xterm-256color",
                    "CLAUDE_CONFIG_DIR": str(config),
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                    "CLAUDE_CODE_NO_FLICKER": "1",
                    "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                }
            )
            if color:
                environment["COLORTERM"] = "truecolor"
            else:
                environment["NO_COLOR"] = "1"
                environment.pop("COLORTERM", None)
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
                "Read,Write,Edit,Bash,Glob,Grep",
                "--session-id",
                session_id,
                "--name",
                "isolated-core-tool-cards-reference",
                "--model",
                "opus",
            ]
            master, slave = pty.openpty()
            fcntl.ioctl(
                slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0)
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
                        [master], [], [], max(0.0, min(0.05, deadline - time.monotonic()))
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
                read(0.2)

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

            def capture(name: str, settle: float = 0.35) -> str:
                current = read(settle)
                (output / f"{name}.txt").write_text(current)
                render_screen(screen, output / f"{name}.png")
                captures.append(name)
                return current

            startup = ""
            for _ in range(4):
                startup = wait_for(
                    "custom API key",
                    "trust this folder",
                    "for shortcuts",
                    "shift+tab",
                    "Try ",
                    seconds=25,
                )
                if "custom API key" in startup:
                    send(b"\x1b[A\r")
                    continue
                if "trust this folder" in startup.lower():
                    send(b"\x1b[B\r")
                    continue
                break
            capture("01-ready")
            send(PROMPT.encode())
            read(0.4)
            send(b"\r")

            # Claude decides on its own which calls need approval, so the
            # driver reacts to whatever dialog appears instead of assuming one
            # prompt per streamed call.
            answered: list[dict[str, str]] = []
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                current = read(0.25)
                if FINAL_TEXT in current:
                    break
                if "Do you want to" not in current:
                    if "sleep 2" in current and "bash-slow-running" not in captures:
                        (output / "bash-slow-running.txt").write_text(current)
                        render_screen(screen, output / "bash-slow-running.png")
                        captures.append("bash-slow-running")
                    continue
                slug = approval_slug(current, len(answered))
                label = f"{len(answered) + 2:02d}-approval-{slug}"
                screen_text = capture(label)
                choices = [
                    (digit, text.strip())
                    for digit, text in re.findall(
                        r"^\s*.?\s*(\d)\.\s+(.*)$", screen_text, re.MULTILINE
                    )
                ]
                if CANCEL_MARKER in screen_text:
                    action = "escape"
                    send(b"\x1b")
                elif DENY_MARKER in screen_text:
                    action = deny_action
                    if deny_action == "escape":
                        send(b"\x1b")
                    else:
                        digit = next(
                            (d for d, text in choices if text.lower() == "no"), "3"
                        )
                        send(digit.encode())
                else:
                    action = "yes"
                    digit = next(
                        (d for d, text in choices if text.lower() == "yes"), "1"
                    )
                    send(digit.encode())
                answered.append({"capture": label, "action": action})
                if action in ("escape", "no"):
                    interrupted = read(0.8)
                    name = "93-cancelled" if action == "escape" else "93-denied"
                    (output / f"{name}.txt").write_text(interrupted)
                    render_screen(screen, output / f"{name}.png")
                    captures.append(name)
                    observed["interrupted_banner"] = "Interrupted" in interrupted
                    break
            observed["approvals"] = answered

            time.sleep(0.8)
            settled = capture("90-completed", settle=0.6)
            observed["completed_has_marker"] = FINAL_TEXT in settled
            send(b"\x0f")
            expanded = capture("91-expanded", settle=0.8)
            observed["verbose_banner"] = "Showing detailed transcript" in expanded
            for page in range(1, 5):
                send(b"\x1b[5~")
                capture(f"91-expanded-scrollback-{page}", settle=0.4)
            for _ in range(5):
                send(b"\x1b[6~")
            send(b"\x0f")
            capture("92-collapsed", settle=0.6)

            assert not (workspace / "denied.txt").exists(), "denied Bash must not run"
            observed["denied_file_absent"] = True
            observed["sample_after_edit"] = (workspace / "sample.txt").read_text()
            observed["notes_written"] = (workspace / "notes.txt").exists()
            result.update(
                {
                    "status": "captured",
                    "claude_version": subprocess.run(
                        [executable, "--version"],
                        capture_output=True,
                        text=True,
                        check=True,
                    ).stdout.strip(),
                    "claude_binary": str(Path(executable).resolve()),
                    "claude_binary_sha256": _sha256(
                        Path(os.path.realpath(executable))
                    ),
                    "fixture_origin": f"http://127.0.0.1:{server.server_port}",
                    "localhost_requests": len(requests),
                    "tool_calls_streamed": len(STEPS),
                    "observed": observed,
                }
            )
    except Exception as error:  # noqa: BLE001 - recorded honestly in result.json
        result.update(
            {"status": "blocked", "error": f"{type(error).__name__}: {error}"}
        )
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
        (output / "fixture-requests.json").write_text(
            json.dumps(requests, indent=2, ensure_ascii=False)
        )
        result["captures"] = captures
        (output / "result.json").write_text(
            json.dumps(result, indent=2, ensure_ascii=False)
        )
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--theme", default="dark", choices=["dark", "light"])
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--deny-action", default="no", choices=["no", "escape"])
    arguments = parser.parse_args()
    print(
        json.dumps(
            run(
                arguments.output,
                columns=arguments.columns,
                rows=arguments.rows,
                timeout=arguments.timeout,
                theme=arguments.theme,
                color=not arguments.no_color,
                deny_action=arguments.deny_action,
            ),
            indent=2,
            ensure_ascii=False,
        )
    )
