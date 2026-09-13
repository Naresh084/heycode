#!/usr/bin/env python3
"""Capture Claude Code `/compact` states with synthetic history and localhost I/O.

No normal conversation prompt is submitted. Each scenario first creates an
isolated Claude session with a local command, then appends synthetic user and
assistant records to that disposable session. The only provider operation is
the `/compact` summary call, forced to a server bound to 127.0.0.1 with a dummy
key after inherited credentials are removed.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
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


FOCUS = "prioritize API decisions, cancellation, and exact filenames"
SYNTHETIC_MARKERS = [
    "SYNTHETIC-ALPHA transport abstraction",
    "SYNTHETIC-BETA crates/example/src/lib.rs",
    "SYNTHETIC-GAMMA cancellation leaves the journal unchanged",
]


class Screen(pyte.Screen):
    def set_mode(self, *modes: int, **kwargs: Any) -> Any:
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class ClaudePty:
    def __init__(
        self,
        executable: str,
        environment: dict[str, str],
        workspace: Path,
        arguments: list[str],
        *,
        columns: int,
        rows: int,
    ) -> None:
        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
        self.process = subprocess.Popen(
            [executable, *arguments],
            cwd=workspace,
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)
        self.master = master
        self.screen = Screen(columns, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.transcript = bytearray()

    def read(self, seconds: float = 0.15) -> str:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select(
                [self.master], [], [], min(0.05, deadline - time.monotonic())
            )
            if not ready:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            self.transcript.extend(data)
            self.stream.feed(data)
        return self.visible()

    def visible(self) -> str:
        return "\n".join(self.screen.display)

    def send(self, data: bytes) -> str:
        os.write(self.master, data)
        return self.read()

    def wait_for(self, *needles: str, timeout: float = 30) -> str:
        deadline = time.monotonic() + timeout
        current = ""
        while time.monotonic() < deadline:
            current = self.read()
            if any(needle.lower() in current.lower() for needle in needles):
                return current
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"Claude exited with {self.process.returncode} while waiting "
                    f"for {needles}:\n{current}"
                )
        raise TimeoutError(f"Timed out waiting for {needles}:\n{current}")

    def wait_event(self, event: threading.Event, label: str, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read()
            if event.is_set():
                return
            if self.process.poll() is not None:
                raise RuntimeError(f"Claude exited while waiting for {label}")
        raise TimeoutError(f"Timed out waiting for {label}:\n{self.visible()}")

    def ready(self) -> str:
        current = ""
        for _ in range(4):
            current = self.wait_for(
                "custom API key",
                "trust this folder",
                "for shortcuts",
                "shift+tab",
                "Try ",
                timeout=20,
            )
            if "custom API key" in current:
                self.send(b"\x1b[A\r")
                continue
            if "trust this folder" in current.lower():
                self.send(b"\x1b[B\r")
                continue
            return current
        return current

    def capture(self, output: Path, name: str) -> str:
        current = self.read(0.35)
        (output / f"{name}.txt").write_text(current)
        render_screen(self.screen, output / f"{name}.png")
        return current

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self.process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.process.wait(timeout=5)
        try:
            os.close(self.master)
        except OSError:
            pass


def run(output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    request_lock = threading.Lock()
    requests: list[dict[str, object]] = []
    state_lock = threading.Lock()
    state = {"scenario": "idle", "mode": "complete"}
    entered = threading.Event()
    release = threading.Event()
    settled = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def json_response(self, status: int, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            if "count_tokens" in self.path:
                with request_lock:
                    requests.append(
                        {
                            "path": self.path,
                            "kind": "count_tokens",
                            "loopback": True,
                        }
                    )
                self.json_response(200, {"input_tokens": 1000})
                return

            with state_lock:
                scenario = state["scenario"]
                mode = state["mode"]
            wire = raw.decode(errors="replace")
            record = {
                "path": self.path,
                "kind": "messages",
                "scenario": scenario,
                "loopback": True,
                "request_bytes": len(raw),
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "tool_count": len(body.get("tools", [])),
                "focus_present": FOCUS in wire,
                "synthetic_markers_present": [
                    marker for marker in SYNTHETIC_MARKERS if marker in wire
                ],
            }
            with request_lock:
                requests.append(record)
            entered.set()

            if mode == "failure":
                self.json_response(
                    400,
                    {
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": "LOCAL_COMPACT_FAILURE",
                        },
                    },
                )
                settled.set()
                return

            if not release.wait(timeout=timeout):
                self.json_response(
                    408,
                    {
                        "type": "error",
                        "error": {
                            "type": "timeout_error",
                            "message": "LOCAL_FIXTURE_RELEASE_TIMEOUT",
                        },
                    },
                )
                settled.set()
                return

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def event(kind: str, value: object) -> None:
                self.wfile.write(
                    f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode()
                )

            try:
                event(
                    "message_start",
                    {
                        "type": "message_start",
                        "message": {
                            "id": f"msg_local_compact_{scenario}",
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
                                "LOCAL COMPACT SUMMARY: preserve the API decision, "
                                "cancellation rule, and crates/example/src/lib.rs."
                            ),
                        },
                    },
                )
                event("content_block_stop", {"type": "content_block_stop", "index": 0})
                event(
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                        "usage": {"output_tokens": 25},
                    },
                )
                event("message_stop", {"type": "message_stop"})
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                record["client_disconnected"] = True
            finally:
                settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    result: dict[str, object] = {
        "status": "failed",
        "scope": "Claude Code compact UI with synthetic persisted history",
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
        "normal_conversation_prompts": 0,
        "credential_source": "dummy loopback key only",
        "scenarios": {},
    }

    try:
        with tempfile.TemporaryDirectory(prefix="claude-compact-reference-") as folder:
            root = Path(folder)
            sandbox_home = root / "home"
            config = root / "config"
            workspace = root / "workspace"
            for path in (sandbox_home, config, workspace):
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
            environment = os.environ.copy()
            for key in list(environment):
                if (
                    key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_"))
                    or key.endswith("_API_KEY")
                    or key.endswith("_AUTH_TOKEN")
                ):
                    environment.pop(key)
            environment.update(
                {
                    "HOME": str(sandbox_home),
                    "TERM": "xterm-256color",
                    "COLORTERM": "truecolor",
                    "CLAUDE_CONFIG_DIR": str(config),
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                    "CLAUDE_CODE_REMOTE_CONTROL": "0",
                    "CLAUDE_CODE_NO_FLICKER": "1",
                    "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                }
            )
            common = [
                "--safe-mode",
                "--strict-mcp-config",
                "--no-chrome",
                "--setting-sources",
                "project,local",
                "--settings",
                '{"remoteControlAtStartup":false}',
                "--permission-mode",
                "manual",
                "--model",
                "opus",
            ]

            for scenario in ("complete", "failure", "cancel"):
                scenario_output = output / scenario
                scenario_output.mkdir()
                session_id = str(uuid.uuid4())
                bootstrap = ClaudePty(
                    executable,
                    environment,
                    workspace,
                    [*common, "--session-id", session_id, "--name", f"compact-{scenario}"],
                    columns=columns,
                    rows=rows,
                )
                try:
                    bootstrap.ready()
                    bootstrap.send(b"/copy\r")
                    bootstrap.wait_for("No assistant message to copy", timeout=20)
                    bootstrap.send(b"/exit\r")
                    deadline = time.monotonic() + 5
                    while bootstrap.process.poll() is None and time.monotonic() < deadline:
                        bootstrap.read()
                finally:
                    bootstrap.close()
                    (scenario_output / "bootstrap.ansi").write_bytes(bootstrap.transcript)

                logs = list(config.glob(f"projects/**/{session_id}.jsonl"))
                assert len(logs) == 1, (scenario, logs)
                log = logs[0]
                seed_session(log, session_id, workspace)
                (scenario_output / "seeded-session.jsonl").write_bytes(log.read_bytes())

                entered.clear()
                release.clear()
                settled.clear()
                with state_lock:
                    state["scenario"] = scenario
                    state["mode"] = "failure" if scenario == "failure" else "complete"

                terminal = ClaudePty(
                    executable,
                    environment,
                    workspace,
                    [*common, "--resume", session_id],
                    columns=columns,
                    rows=rows,
                )
                ui_evidence: dict[str, bool] = {}
                try:
                    terminal.ready()
                    terminal.capture(scenario_output, "00-ready-with-synthetic-history")
                    terminal.send(f"/compact {FOCUS}".encode())
                    terminal.read(0.5)
                    terminal.send(b"\r")
                    terminal.wait_event(entered, "localhost compact request", timeout)

                    if scenario == "complete":
                        working = terminal.capture(scenario_output, "01-working")
                        assert "Compacting conversation" in working, working
                        assert "esc to interrupt" in working, working
                        ui_evidence["working_progress_and_interrupt_hint"] = True
                        release.set()
                        terminal.wait_event(settled, "localhost compact completion", timeout)
                        terminal.wait_for("Compacted", "compact", timeout=20)
                        terminal.read(1)
                        completed = terminal.capture(scenario_output, "02-completed")
                        assert "Compacted (ctrl+o to see full summary)" in completed, completed
                        ui_evidence["completed_receipt_and_summary_hint"] = True
                    elif scenario == "failure":
                        terminal.wait_event(settled, "localhost compact failure", timeout)
                        terminal.wait_for("LOCAL_COMPACT_FAILURE", "API Error", "failed", timeout=20)
                        failed = terminal.capture(scenario_output, "01-failed")
                        assert (
                            "Error during compaction: API Error: 400 LOCAL_COMPACT_FAILURE"
                            in failed
                        ), failed
                        ui_evidence["explicit_failure_receipt"] = True
                    else:
                        working = terminal.capture(scenario_output, "01-working")
                        assert "Compacting conversation" in working, working
                        assert "esc to interrupt" in working, working
                        ui_evidence["working_progress_and_interrupt_hint"] = True
                        terminal.send(b"\x1b")
                        terminal.read(0.8)
                        release.set()
                        terminal.wait_event(settled, "cancelled localhost request settlement", timeout)
                        terminal.read(1)
                        cancelled = terminal.capture(scenario_output, "02-cancelled")
                        assert "Compacting conversation" not in cancelled, cancelled
                        assert f"/compact {FOCUS}" in cancelled, cancelled
                        ui_evidence["cancel_restores_command_to_composer"] = True
                finally:
                    (scenario_output / "terminal.ansi").write_bytes(terminal.transcript)
                    terminal.close()

                final_bytes = log.read_bytes()
                seeded_bytes = (scenario_output / "seeded-session.jsonl").read_bytes()
                assert final_bytes.startswith(seeded_bytes)
                final_records = [
                    json.loads(line) for line in final_bytes.decode().splitlines() if line
                ]
                compact_boundaries = [
                    record
                    for record in final_records
                    if record.get("type") == "system"
                    and record.get("subtype") == "compact_boundary"
                ]
                (scenario_output / "final-session.jsonl").write_bytes(final_bytes)
                scenario_requests = [
                    request
                    for request in requests
                    if request.get("scenario") == scenario
                    and request.get("kind") == "messages"
                ]
                expected_attempts = 2 if scenario == "failure" else 1
                assert len(scenario_requests) == expected_attempts, scenario_requests
                assert all(
                    request["focus_present"] is True for request in scenario_requests
                )
                assert all(
                    set(request["synthetic_markers_present"]) == set(SYNTHETIC_MARKERS)
                    for request in scenario_requests
                )
                request_hashes = {
                    request["request_sha256"] for request in scenario_requests
                }
                assert len(request_hashes) == 1, scenario_requests
                if scenario == "complete":
                    assert len(compact_boundaries) == 1, compact_boundaries
                else:
                    assert not compact_boundaries, compact_boundaries
                result["scenarios"][scenario] = {
                    "session_id": session_id,
                    "message_requests": len(scenario_requests),
                    "identical_request_retries": len(scenario_requests) - 1,
                    "request_sha256": next(iter(request_hashes)),
                    "focus_present": scenario_requests[0]["focus_present"],
                    "synthetic_markers_present": scenario_requests[0][
                        "synthetic_markers_present"
                    ],
                    "compact_boundaries": len(compact_boundaries),
                    "seeded_prefix_byte_exact": True,
                    "ui_evidence": ui_evidence,
                    "client_disconnected": scenario_requests[0].get(
                        "client_disconnected", False
                    ),
                }

            result.update(
                {
                    "status": "passed",
                    "claude_version": subprocess.run(
                        [executable, "--version"],
                        capture_output=True,
                        text=True,
                        check=True,
                    ).stdout.strip(),
                    "claude_binary": str(Path(executable).resolve()),
                    "claude_binary_sha256": sha256(Path(executable).resolve()),
                    "localhost_requests": len(requests),
                    "message_requests": sum(
                        request.get("kind") == "messages" for request in requests
                    ),
                    "count_token_requests": sum(
                        request.get("kind") == "count_tokens" for request in requests
                    ),
                }
            )
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        (output / "requests-summary.json").write_text(json.dumps(requests, indent=2))
        (output / "result.json").write_text(json.dumps(result, indent=2))

    print(json.dumps(result, indent=2))
    return result


def seed_session(log: Path, session_id: str, workspace: Path) -> None:
    records = [json.loads(line) for line in log.read_text().splitlines() if line]
    parent = next(
        (record["uuid"] for record in reversed(records) if "uuid" in record), None
    )
    now = "2026-09-11T00:00:00.000Z"
    seeded: list[dict[str, object]] = []
    for index, marker in enumerate(SYNTHETIC_MARKERS):
        user_id = str(uuid.uuid4())
        assistant_id = str(uuid.uuid4())
        common = {
            "isSidechain": False,
            "userType": "external",
            "cwd": str(workspace.resolve()),
            "sessionId": session_id,
            "version": "2.1.268",
            "gitBranch": "",
            "timestamp": now,
            "fixtureSeeded": True,
        }
        seeded.append(
            {
                **common,
                "parentUuid": parent,
                "type": "user",
                "message": {"role": "user", "content": marker},
                "uuid": user_id,
            }
        )
        seeded.append(
            {
                **common,
                "parentUuid": user_id,
                "type": "assistant",
                "message": {
                    "model": "fixture-local-no-inference",
                    "id": f"msg_fixture_{index}",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "text",
                            "text": f"Synthetic answer {index}: {marker} retained.",
                        }
                    ],
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {
                        "input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "output_tokens": 0,
                    },
                },
                "uuid": assistant_id,
                "requestId": f"req_fixture_{index}",
            }
        )
        parent = assistant_id
    with log.open("a") as stream:
        for record in seeded:
            stream.write(json.dumps(record, separators=(",", ":")) + "\n")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=50)
    parser.add_argument("--timeout", type=float, default=30)
    arguments = parser.parse_args()
    outcome = run(
        arguments.output,
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
    raise SystemExit(0 if outcome["status"] == "passed" else 1)
