#!/usr/bin/env python3
"""Actual-PTY evidence for `/exit` while a provider turn is active.

Both CLIs run in disposable homes and talk only to deterministic HTTP servers
bound to 127.0.0.1.  The servers begin a streaming response and deliberately
leave it open so the real terminal clients have active work to interrupt.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

from memory_skills_pty import MODEL
from terminal_screenshot import TerminalByteStream
from workspace_transitions_pty import Terminal


PROMPT = "ACTIVE_EXIT_LOCALHOST_FIXTURE"


def clean_environment(home: Path) -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not (
            key.startswith(
                (
                    "ANTHROPIC_",
                    "CLAUDE_CODE_OAUTH",
                    "CLAUDE_CODE_USE_",
                )
            )
            or key.endswith(("_API_KEY", "_AUTH_TOKEN", "_ACCESS_TOKEN"))
        )
    }
    environment.update(
        {
            "HOME": str(home),
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "CLAUDE_CODE_REMOTE_CONTROL": "0",
            "CLAUDE_CODE_NO_FLICKER": "1",
        }
    )
    environment.pop("NO_COLOR", None)
    return environment


def wait_for_event(
    terminal: Terminal,
    event: threading.Event,
    label: str,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        terminal.read(0.1)
        if event.is_set():
            return
        if terminal.process.poll() is not None:
            raise AssertionError(
                f"CLI exited while waiting for {label}:\n{terminal.visible()}"
            )
    raise AssertionError(f"Timed out waiting for {label}:\n{terminal.visible()}")


def wait_for_exit(terminal: Terminal, timeout: float) -> tuple[int | None, float]:
    started = time.monotonic()
    deadline = started + timeout
    while terminal.process.poll() is None and time.monotonic() < deadline:
        terminal.read(0.1)
    return terminal.process.poll(), time.monotonic() - started


def ready(terminal: Terminal) -> str:
    for _ in range(8):
        terminal.read(0.5)
        visible = terminal.visible()
        if "custom API key" in visible:
            os.write(terminal.master, b"\x1b[A\r")
        elif "trust this folder" in visible.lower():
            os.write(terminal.master, b"\x1b[B\r")
        elif any(
            marker in visible.lower()
            for marker in ("for shortcuts", "shift+tab", "try ", "context")
        ):
            return visible
    raise AssertionError(f"No idle composer:\n{terminal.visible()}")


def stream_openai_chunk(handler: http.server.BaseHTTPRequestHandler) -> None:
    chunk = {
        "id": "terminal-active-exit",
        "object": "chat.completion.chunk",
        "model": MODEL["id"],
        "choices": [
            {
                "index": 0,
                "delta": {"reasoning": "LOCAL ACTIVE TURN HELD"},
                "finish_reason": None,
            }
        ],
    }
    handler.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
    handler.wfile.flush()


def finish_openai_stream(handler: http.server.BaseHTTPRequestHandler) -> None:
    chunk = {
        "id": "terminal-active-exit",
        "object": "chat.completion.chunk",
        "model": MODEL["id"],
        "choices": [
            {
                "index": 0,
                "delta": {"content": "LOCAL RELEASE"},
                "finish_reason": "stop",
            }
        ],
    }
    handler.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
    handler.wfile.write(b"data: [DONE]\n\n")
    handler.wfile.flush()


def run_heycode(binary: Path, output: Path, timeout: float) -> dict[str, object]:
    output.mkdir(parents=True)
    entered = threading.Event()
    release = threading.Event()
    settled = threading.Event()
    requests: list[dict[str, object]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def send_json(self, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            if self.path.endswith("/key"):
                self.send_json({"data": {"label": "terminal-exit-local"}})
            elif "/model/" in self.path:
                self.send_json({"data": MODEL})
            else:
                self.send_json(
                    {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                )

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            record: dict[str, object] = {
                "path": self.path,
                "loopback": self.client_address[0] == "127.0.0.1",
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "prompt_present": PROMPT in raw.decode(errors="replace"),
                "exit_command_present": "/exit" in raw.decode(errors="replace"),
            }
            requests.append(record)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            try:
                stream_openai_chunk(self)
                entered.set()
                release.wait(timeout=timeout + 10)
                finish_openai_stream(self)
            except (BrokenPipeError, ConnectionResetError):
                record["client_disconnected"] = True
            finally:
                settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    terminal: Terminal | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-active-exit-") as folder:
            root = Path(folder).resolve()
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "settings.toml").write_text("schema_version = 1\n")
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_ACTIVE_EXIT_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            environment = clean_environment(home)
            environment.update(
                {
                    "HEYCODE_HOME": str(home),
                    "HEYCODE_ACTIVE_EXIT_FIXTURE": "local-only-not-a-credential",
                }
            )
            terminal = Terminal(
                [
                    str(binary),
                    "--no-background",
                    "--trust-workspace",
                    "--provider",
                    "openrouter",
                    "--model",
                    str(MODEL["id"]),
                    "--set",
                    f"llm.base_url={base}",
                    "--set",
                    "llm.api_key_env=HEYCODE_ACTIVE_EXIT_FIXTURE",
                ],
                workspace,
                environment,
                output,
            )
            terminal.stream = TerminalByteStream(terminal.screen)
            ready(terminal)
            terminal.capture("00-ready")
            terminal.send(PROMPT)
            wait_for_event(terminal, entered, "localhost provider request", timeout)
            active = terminal.capture("01-active-turn")
            assert "LOCAL ACTIVE TURN HELD" in active, active

            terminal.send("/exit")
            confirmation = terminal.wait("Interrupt active work?", timeout)
            terminal.capture("02-cancel-default")
            assert "Cancel" in confirmation and "Interrupt & run" in confirmation
            assert terminal.process.poll() is None

            os.write(terminal.master, b"\x1b")
            terminal.read(0.4)
            restored = terminal.capture("03-escape-restores-draft")
            assert "Interrupt active work?" not in restored
            assert "/exit" in restored
            assert terminal.process.poll() is None
            assert len(requests) == 1

            os.write(terminal.master, b"\r")
            terminal.wait("Interrupt active work?", timeout)
            os.write(terminal.master, b"\x1b[C")
            selected = terminal.capture("04-interrupt-selected")
            assert "Interrupt & run" in selected
            os.write(terminal.master, b"\r")
            terminal.read(0.15)
            terminal.capture("05-interrupt-requested")
            exit_code, exit_seconds = wait_for_exit(terminal, 9)
            released_to_unblock = False
            if exit_code is None:
                released_to_unblock = True
                release.set()
                exit_code, extra = wait_for_exit(terminal, 5)
                exit_seconds += extra
            terminal.capture("06-exited")
            assert exit_code == 0, (exit_code, terminal.visible())
            assert not released_to_unblock, "quit waited for fixture response completion"

            release.set()
            settled.wait(timeout=3)
            journal_files = list((home / "sessions").rglob("session.jsonl"))
            assert len(journal_files) == 1, journal_files
            journal_text = journal_files[0].read_text()
            (output / "session.jsonl").write_text(journal_text)
            events = [json.loads(line) for line in journal_text.splitlines() if line]
            kinds = [event.get("kind") for event in events]
            assert kinds.count("user/message") == 1, kinds
            assert kinds.count("turn/start") == 1, kinds
            assert kinds.count("turn/end") == 1, kinds
            assert "/exit" not in journal_text
            assert len(requests) == 1, requests
            assert requests[0]["prompt_present"] is True
            assert requests[0]["exit_command_present"] is False
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            return {
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "provider": "deterministic OpenAI-compatible SSE on 127.0.0.1",
                "provider_requests": len(requests),
                "commercial_provider_requests": 0,
                "confirmation_cancel_default": True,
                "escape_restored_exact_command": True,
                "interrupt_selected_then_queued": True,
                "process_exit_code": exit_code,
                "exit_after_confirmation_seconds": round(exit_seconds, 3),
                "fixture_release_needed_for_exit": released_to_unblock,
                "external_termination_used": False,
                "journal_event_kinds": kinds,
                "exit_command_in_model_request": False,
                "exit_command_in_journal": False,
                "captures": terminal.captures,
            }
    finally:
        release.set()
        if terminal is not None:
            terminal.close()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)


def anthropic_event(
    handler: http.server.BaseHTTPRequestHandler,
    event: str,
    data: object,
) -> None:
    handler.wfile.write(
        f"event: {event}\ndata: {json.dumps(data)}\n\n".encode()
    )
    handler.wfile.flush()


def run_claude(output: Path, timeout: float) -> dict[str, object]:
    output.mkdir(parents=True)
    executable_name = shutil.which("claude")
    if executable_name is None:
        raise AssertionError("Claude CLI is not installed")
    executable = Path(executable_name).resolve()
    entered = threading.Event()
    release = threading.Event()
    settled = threading.Event()
    requests: list[dict[str, object]] = []
    network_attempts: list[dict[str, object]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def send_json(self, status: int, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_CONNECT(self) -> None:  # noqa: N802 - stdlib callback name
            network_attempts.append({"method": "CONNECT", "path": self.path})
            self.send_json(503, {"error": "loopback harness refuses proxy traffic"})

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            network_attempts.append({"method": "GET", "path": self.path})
            self.send_json(503, {"error": "loopback harness refuses auxiliary traffic"})

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            if "count_tokens" in self.path:
                requests.append({"path": self.path, "kind": "count_tokens"})
                self.send_json(200, {"input_tokens": 1000})
                return
            wire = raw.decode(errors="replace")
            record: dict[str, object] = {
                "path": self.path,
                "kind": "messages",
                "loopback": self.client_address[0] == "127.0.0.1",
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "prompt_present": PROMPT in wire,
                "exit_command_present": "/exit" in wire,
            }
            requests.append(record)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            try:
                anthropic_event(
                    self,
                    "message_start",
                    {
                        "type": "message_start",
                        "message": {
                            "id": "msg_terminal_active_exit",
                            "type": "message",
                            "role": "assistant",
                            "model": "claude-opus-5-local-fixture",
                            "content": [],
                            "stop_reason": None,
                            "stop_sequence": None,
                            "usage": {
                                "input_tokens": 1000,
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": 0,
                                "output_tokens": 0,
                            },
                        },
                    },
                )
                anthropic_event(
                    self,
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""},
                    },
                )
                anthropic_event(
                    self,
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "text_delta",
                            "text": "LOCAL ACTIVE TURN HELD",
                        },
                    },
                )
                entered.set()
                release.wait(timeout=timeout + 10)
                anthropic_event(
                    self,
                    "content_block_stop",
                    {"type": "content_block_stop", "index": 0},
                )
                anthropic_event(
                    self,
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                        "usage": {"output_tokens": 5},
                    },
                )
                anthropic_event(self, "message_stop", {"type": "message_stop"})
            except (BrokenPipeError, ConnectionResetError):
                record["client_disconnected"] = True
            finally:
                settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    terminal: Terminal | None = None
    cleanup_external = False
    try:
        with tempfile.TemporaryDirectory(prefix="claude-active-exit-") as folder:
            root = Path(folder).resolve()
            home = root / "home"
            config = root / "config"
            workspace = root / "workspace"
            for path in (home, config, workspace):
                path.mkdir()
            (config / ".claude.json").write_text(
                json.dumps(
                    {
                        "hasCompletedOnboarding": True,
                        "theme": "dark",
                        "lastOnboardingVersion": "2.1.268",
                    }
                )
            )
            endpoint = f"http://127.0.0.1:{server.server_port}"
            environment = clean_environment(home)
            environment.update(
                {
                    "CLAUDE_CONFIG_DIR": str(config),
                    "ANTHROPIC_BASE_URL": endpoint,
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                    "HTTP_PROXY": endpoint,
                    "HTTPS_PROXY": endpoint,
                    "NO_PROXY": "localhost,127.0.0.1",
                }
            )
            terminal = Terminal(
                [
                    str(executable),
                    "--safe-mode",
                    "--restricted",
                    "--strict-mcp-config",
                    "--no-chrome",
                    "--permission-mode",
                    "manual",
                    "--setting-sources",
                    "project,local",
                    "--settings",
                    '{"remoteControlAtStartup":false}',
                    "--tools",
                    "",
                    "--model",
                    "opus",
                    "--session-id",
                    str(uuid.uuid4()),
                ],
                workspace,
                environment,
                output,
            )
            terminal.stream = TerminalByteStream(terminal.screen)
            ready(terminal)
            terminal.capture("00-ready")
            terminal.send(PROMPT)
            wait_for_event(terminal, entered, "Claude localhost provider request", timeout)
            active = terminal.capture("01-active-turn")
            assert "esc to interrupt" in active.lower(), active
            assert PROMPT in active, active

            os.write(terminal.master, b"/exit")
            terminal.read(0.6)
            draft = terminal.capture("02-active-exit-draft")
            assert terminal.process.poll() is None, "typing /exit exited Claude early"
            assert "/exit" in draft
            os.write(terminal.master, b"\r")
            exit_code, exit_seconds = wait_for_exit(terminal, 5)
            terminal.capture("03-after-active-exit-submit")
            active_exit_completed = exit_code == 0
            if exit_code is None:
                # Escape is a normal Claude UI cancellation input.  It bounds
                # cleanup without making a process-level termination claim.
                os.write(terminal.master, b"\x1b")
                terminal.read(1)
                os.write(terminal.master, b"\x15/exit\r")
                exit_code, extra = wait_for_exit(terminal, 8)
                exit_seconds += extra
            cleanup_external = exit_code is None
            terminal.capture("04-exited")
            assert exit_code == 0, (exit_code, terminal.visible())
            release.set()
            settled.wait(timeout=3)

            message_requests = [
                request for request in requests if request.get("kind") == "messages"
            ]
            assert message_requests, requests
            assert message_requests[0]["prompt_present"] is True
            assert not any(
                request["exit_command_present"] for request in message_requests
            ), message_requests
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            journals = list(config.glob("projects/**/*.jsonl"))
            if journals:
                (output / "session.jsonl").write_text(journals[0].read_text())
            version = subprocess.run(
                [str(executable), "--version"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
            return {
                "binary": str(executable),
                "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
                "version": version,
                "provider": "deterministic Anthropic SSE on 127.0.0.1",
                "provider_requests": len(message_requests),
                "commercial_provider_requests": 0,
                "typed_draft_kept_process_alive": True,
                "active_exit_completed_without_prior_escape": active_exit_completed,
                "process_exit_code": exit_code,
                "exit_after_submit_seconds": round(exit_seconds, 3),
                "external_process_termination_used": cleanup_external,
                "exit_command_in_first_model_request": message_requests[0][
                    "exit_command_present"
                ],
                "network_attempts": network_attempts,
                "captures": terminal.captures,
            }
    finally:
        release.set()
        if terminal is not None:
            terminal.close()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=35)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    result: dict[str, object] = {
        "status": "failed",
        "scope": "active-turn /exit lifecycle in actual isolated PTYs",
        "controlled_localhost_sessions": 2,
        "external_provider_requests": 0,
        "commercial_provider_requests": 0,
    }
    try:
        result["heycode"] = run_heycode(
            args.binary.resolve(), output / "heycode", args.timeout
        )
        result["claude"] = run_claude(output / "claude", args.timeout)
        result["localhost_provider_requests"] = (
            result["heycode"]["provider_requests"]
            + result["claude"]["provider_requests"]
        )
        result["status"] = "passed"
    except Exception as error:
        result["status"] = "blocked"
        result["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        (output / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
