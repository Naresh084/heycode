#!/usr/bin/env python3
"""Exercise durable scheduler tools through the real CLI TUI and a local provider.

The fixture uses one immutable heycode binary, a loopback OpenRouter-shaped HTTP
server, and disposable home/workspace directories. It makes no external model
request and never reads or writes the user's real heycode state.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import traceback

import pyte

from task_console_pty import MODEL
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MARKERS = (
    "SCHEDULER_INVALID_CRON",
    "SCHEDULER_CREATE_ONE_SHOT",
    "SCHEDULER_ONE_SHOT_FIRED",
    "SCHEDULER_CREATE_CRON",
    "SCHEDULER_AFTER_RESTART",
    "SCHEDULER_DELETE_CRON",
    "SCHEDULER_WAKEUP_FLOW",
)


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tool_result(message: dict) -> object:
    content = message.get("content", "")
    if not isinstance(content, str):
        return content
    try:
        return json.loads(content)
    except json.JSONDecodeError:
        return content


def latest_fixture_turn(messages: list[dict]) -> tuple[str | None, dict | None]:
    latest_index = -1
    marker = None
    for index, message in enumerate(messages):
        if message.get("role") != "user":
            continue
        content = str(message.get("content", ""))
        for candidate in MARKERS:
            if candidate in content:
                latest_index = index
                marker = candidate
    if marker is None:
        return None, None
    tail = [message for message in messages[latest_index + 1 :] if message.get("role") == "tool"]
    return marker, tail[-1] if tail else None


def run(binary: Path, output: Path) -> None:
    binary = binary.resolve()
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict] = []
    handler_failures: list[str] = []
    state: dict[str, object] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path.endswith("/key"):
                payload = {"data": {"label": "local scheduler fixture"}}
            elif "/model/" in self.path:
                payload = {"data": MODEL}
            else:
                payload = {
                    "data": [MODEL],
                    "total_count": 1,
                    "links": {"next": None},
                }
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta: dict, finish: str | None = None) -> None:
                payload = {
                    "id": "scheduler-pty-fixture",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {
                            "index": 0,
                            "delta": delta,
                            "finish_reason": finish,
                        }
                    ],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                self.wfile.flush()

            def call(name: str, arguments: dict, call_id: str) -> None:
                chunk(
                    {
                        "reasoning": "Exercise the actual durable scheduler tool.",
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": json.dumps(arguments),
                                },
                            }
                        ],
                    }
                )
                chunk({}, "tool_calls")

            def finish(text: str) -> None:
                chunk({"content": text})
                chunk({}, "stop")

            try:
                marker, last_tool = latest_fixture_turn(request.get("messages", []))
                call_id = None if last_tool is None else last_tool.get("tool_call_id")
                result = None if last_tool is None else tool_result(last_tool)

                if marker == "SCHEDULER_INVALID_CRON":
                    if last_tool is None:
                        call(
                            "schedule_create",
                            {
                                "prompt": "must never be admitted",
                                "cron": "*/0 * * * *",
                                "recurring": True,
                            },
                            "scheduler-invalid-cron",
                        )
                    else:
                        state["invalid_cron_result"] = result
                        finish("INVALID_CRON_REJECTED")
                elif marker == "SCHEDULER_CREATE_ONE_SHOT":
                    if last_tool is None:
                        call(
                            "schedule_create",
                            {
                                "prompt": "SCHEDULER_ONE_SHOT_FIRED",
                                "after_seconds": 1,
                            },
                            "scheduler-one-shot-create",
                        )
                    else:
                        state["one_shot_create"] = result
                        state["one_shot_id"] = result["schedule_id"]
                        finish("ONE_SHOT_CREATED")
                elif marker == "SCHEDULER_ONE_SHOT_FIRED":
                    state["one_shot_delivery_count"] = (
                        int(state.get("one_shot_delivery_count", 0)) + 1
                    )
                    state["one_shot_delivery_request"] = len(requests)
                    state["one_shot_delivery_input"] = next(
                        str(message.get("content", ""))
                        for message in reversed(request.get("messages", []))
                        if message.get("role") == "user"
                        and "SCHEDULER_ONE_SHOT_FIRED"
                        in str(message.get("content", ""))
                    )
                    finish("ONE_SHOT_FIRED_EXACTLY_ONCE")
                elif marker == "SCHEDULER_CREATE_CRON":
                    if last_tool is None:
                        call(
                            "schedule_create",
                            {
                                "prompt": "check the local scheduler",
                                "cron": "7 * * * *",
                                "recurring": True,
                            },
                            "scheduler-cron-create",
                        )
                    elif call_id == "scheduler-cron-create":
                        state["cron_create"] = result
                        state["cron_id"] = result["schedule_id"]
                        call("schedule_list", {}, "scheduler-cron-list")
                    elif call_id == "scheduler-cron-list":
                        state["cron_list"] = result
                        finish("CRON_CREATED_AND_LISTED")
                    else:
                        raise AssertionError(f"unexpected cron stage: {call_id}")
                elif marker == "SCHEDULER_AFTER_RESTART":
                    if last_tool is None:
                        call("schedule_list", {}, "scheduler-resume-list")
                    else:
                        state["resume_list"] = result
                        finish("CRON_RESTORED_AFTER_RESTART")
                elif marker == "SCHEDULER_DELETE_CRON":
                    if last_tool is None:
                        call(
                            "schedule_delete",
                            {"schedule_id": state["cron_id"]},
                            "scheduler-cron-delete",
                        )
                    elif call_id == "scheduler-cron-delete":
                        state["cron_delete"] = result
                        call("schedule_list", {}, "scheduler-after-delete-list")
                    elif call_id == "scheduler-after-delete-list":
                        state["after_delete_list"] = result
                        finish("CRON_DELETED_AND_EMPTY")
                    else:
                        raise AssertionError(f"unexpected delete stage: {call_id}")
                elif marker == "SCHEDULER_WAKEUP_FLOW":
                    if last_tool is None:
                        call(
                            "schedule_create",
                            {
                                "prompt": "self-paced local check",
                                "wakeup_seconds": 60,
                            },
                            "scheduler-wakeup-create",
                        )
                    elif call_id == "scheduler-wakeup-create":
                        state["wakeup_create"] = result
                        state["wakeup_id"] = result["schedule_id"]
                        call(
                            "schedule_wakeup",
                            {
                                "schedule_id": state["wakeup_id"],
                                "delay_seconds": 120,
                            },
                            "scheduler-wakeup-reschedule",
                        )
                    elif call_id == "scheduler-wakeup-reschedule":
                        state["wakeup_reschedule"] = result
                        call(
                            "schedule_wakeup",
                            {"schedule_id": state["wakeup_id"], "stop": True},
                            "scheduler-wakeup-stop",
                        )
                    elif call_id == "scheduler-wakeup-stop":
                        state["wakeup_stop"] = result
                        call("schedule_list", {}, "scheduler-after-wakeup-list")
                    elif call_id == "scheduler-after-wakeup-list":
                        state["after_wakeup_list"] = result
                        finish("WAKEUP_RESCHEDULED_STOPPED_AND_EMPTY")
                    else:
                        raise AssertionError(f"unexpected wakeup stage: {call_id}")
                else:
                    finish("SCHEDULER_FIXTURE_READY")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception:
                handler_failures.append(traceback.format_exc())
                try:
                    finish("SCHEDULER_FIXTURE_HANDLER_ERROR")
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    os.environ["HEYCODE_SCHEDULER_PTY_FIXTURE"] = "local-only-not-a-real-credential"

    captures: list[str] = []
    transcript = bytearray()
    tui: FullScreenTui | None = None
    screen = Screen(120, 44)
    stream = TerminalByteStream(screen)

    def read(seconds: float = 0.15) -> str:
        if tui is None:
            return ""
        stream.feed(tui.read(seconds))
        return "\n".join(screen.display)

    def visible() -> str:
        return "\n".join(screen.display)

    def wait(needle: str, timeout: float = 35) -> str:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text = read()
            if needle in text:
                return text
            if tui is not None and not tui.alive():
                raise AssertionError(f"CLI exited while waiting for {needle!r}:\n{text}")
        raise AssertionError(f"timed out waiting for {needle!r}:\n{visible()}")

    def send(value: bytes) -> None:
        if tui is None:
            raise AssertionError("CLI is not running")
        os.write(tui.fd, value)
        read()

    def command(value: str, expected: str) -> str:
        send(value.encode())
        send(b"\r")
        return wait(expected)

    def capture(name: str) -> str:
        text = read(0.35)
        (output / f"{name}.txt").write_text(text)
        render_screen(screen, output / f"{name}.png")
        captures.append(name)
        return text

    def start(home: Path, workspace: Path, *, resume: bool = False) -> None:
        nonlocal tui, screen, stream
        screen = Screen(120, 44)
        stream = TerminalByteStream(screen)
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        extra = [
            "--provider",
            "openrouter",
            "--model",
            MODEL["id"],
            "--approval",
            "full_access",
            "--set",
            f"llm.base_url={base}",
            "--set",
            "llm.api_key_env=HEYCODE_SCHEDULER_PTY_FIXTURE",
        ]
        if resume:
            extra.insert(0, "--continue")
        tui = FullScreenTui(
            str(home),
            str(workspace),
            str(binary),
            fake=False,
            color=True,
            rows=44,
            columns=120,
            extra=extra,
        )
        initial = read(2)
        if "Welcome to heycode" in initial:
            send(b"\x1b[B\x1b[B\r")
            wait("Select a provider")
            send(b"OpenRouter\r")
            wait("Paste your OpenRouter API key")
            send(b"local-only-not-a-real-credential\r")
            wait("Choose a model")
            send(b"\r")
        wait("full access on")

    def stop() -> None:
        nonlocal tui
        if tui is None:
            return
        running = tui
        if running.alive():
            try:
                os.write(running.fd, b"/quit\r")
            except OSError:
                pass
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and running.alive():
                read(0.1)
        transcript.extend(running.transcript)
        running.close()
        tui = None

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-scheduler-pty-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "settings.toml").write_text("schema_version = 1\n")
            (home / "config.toml").write_text(
                "schema_version = 30\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_SCHEDULER_PTY_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )

            start(home, workspace)
            command("SCHEDULER_INVALID_CRON", "INVALID_CRON_REJECTED")
            capture("01-invalid-cron-rejected")
            command("SCHEDULER_CREATE_ONE_SHOT", "ONE_SHOT_CREATED")
            wait("ONE_SHOT_FIRED_EXACTLY_ONCE", timeout=12)
            reminder_frame = capture("02-one-shot-fired")
            assert "Scheduled reminder: SCHEDULER_ONE_SHOT_FIRED" in reminder_frame
            assert "[SCHEDULE REMINDER]" not in reminder_frame
            assert "reminder_prompt_json:" not in reminder_frame
            assert state["one_shot_delivery_count"] == 1
            settled_requests = len(requests)
            read(2.0)
            assert len(requests) == settled_requests
            assert state["one_shot_delivery_count"] == 1
            command("SCHEDULER_CREATE_CRON", "CRON_CREATED_AND_LISTED")
            capture("03-cron-tool-flow")
            command_requests = len(requests)
            listed = command("/schedule list", "Local schedules: 1")
            assert state["cron_id"] in listed
            assert len(requests) == command_requests
            capture("04-command-list-before-restart")
            stop()

            start(home, workspace, resume=True)
            restart_requests = len(requests)
            read(2.0)
            assert len(requests) == restart_requests
            assert state["one_shot_delivery_count"] == 1
            command_requests = len(requests)
            listed = command("/schedule list", "Local schedules: 1")
            assert state["cron_id"] in listed
            assert len(requests) == command_requests
            capture("05-command-list-after-restart")
            command("SCHEDULER_AFTER_RESTART", "CRON_RESTORED_AFTER_RESTART")
            capture("06-tool-list-after-restart")
            command("SCHEDULER_DELETE_CRON", "CRON_DELETED_AND_EMPTY")
            capture("07-cron-delete")
            command_requests = len(requests)
            command("/schedule list", "No active local schedules.")
            assert len(requests) == command_requests
            capture("08-empty-after-delete")
            command("SCHEDULER_WAKEUP_FLOW", "WAKEUP_RESCHEDULED_STOPPED_AND_EMPTY")
            capture("09-wakeup-reschedule-stop")
            command_requests = len(requests)
            command("/schedule list", "No active local schedules.")
            assert len(requests) == command_requests
            capture("10-empty-after-wakeup-stop")
            stop()

            start(home, workspace, resume=True)
            restart_requests = len(requests)
            read(2.0)
            assert len(requests) == restart_requests
            assert state["one_shot_delivery_count"] == 1
            command_requests = len(requests)
            command("/schedule list", "No active local schedules.")
            assert len(requests) == command_requests
            capture("11-empty-after-second-restart")

            logs = sorted(home.rglob("session.jsonl"))
            assert len(logs) == 1, logs
            events = [
                json.loads(line)
                for line in logs[0].read_text().splitlines()
                if line.strip()
            ]
            changes = [
                event["data"]["change"]
                for event in events
                if event.get("kind") == "schedule/change"
            ]
            operations = [change["operation"] for change in changes]

            one_shot_id = state["one_shot_id"]
            one_shot_creates = [
                change
                for change in changes
                if change["operation"] == "create"
                and change["schedule"]["id"] == one_shot_id
            ]
            one_shot_dispatches = [
                change
                for change in changes
                if change["operation"] == "dispatch" and change["id"] == one_shot_id
            ]
            one_shot_enqueues = [
                (event["seq"], message)
                for event in events
                if event.get("kind") == "agent/inbox/splice"
                for message in event["data"].get("inserted", [])
                if message.get("source", {}).get("kind") == "schedule"
                and message["source"].get("schedule_id") == one_shot_id
            ]

            assert not handler_failures, handler_failures
            invalid = str(state["invalid_cron_result"]).lower()
            assert "invalid" in invalid, state["invalid_cron_result"]
            one_shot_create = state["one_shot_create"]
            assert one_shot_create["kind"] == "after"
            assert one_shot_create["schedule_id"] == one_shot_id
            assert state["one_shot_delivery_count"] == 1
            assert len(one_shot_creates) == 1, one_shot_creates
            assert len(one_shot_enqueues) == 1, one_shot_enqueues
            assert len(one_shot_dispatches) == 1, one_shot_dispatches
            one_shot_schedule = one_shot_creates[0]["schedule"]
            one_shot_message = one_shot_enqueues[0][1]
            one_shot_dispatch = one_shot_dispatches[0]
            assert (
                one_shot_message["source"]["occurrence_at_ms"]
                == one_shot_schedule["scheduled_at_ms"]
            )
            assert one_shot_dispatch["message_id"] == one_shot_message["id"]
            assert (
                one_shot_dispatch["accepted_at_ms"]
                >= one_shot_schedule["scheduled_at_ms"]
            )
            cron_create = state["cron_create"]
            assert cron_create["kind"] == "cron"
            assert cron_create["cron"] == "7 * * * *"
            assert cron_create["timezone"] == "local"
            assert cron_create["state"] == "scheduled"
            assert cron_create["owner"] == "session"
            assert cron_create["delivery_mode"] == "session_local"
            assert cron_create["restored_on_resume"] is True
            assert cron_create["expires_at_ms"] > cron_create["created_at_ms"]
            assert 0 <= cron_create["jitter_ms"] <= 30 * 60 * 1000
            assert [row["schedule_id"] for row in state["cron_list"]] == [
                state["cron_id"]
            ]
            assert [row["schedule_id"] for row in state["resume_list"]] == [
                state["cron_id"]
            ]
            assert state["cron_delete"]["deleted"] is True
            assert state["after_delete_list"] == []

            wakeup_create = state["wakeup_create"]
            wakeup_reschedule = state["wakeup_reschedule"]
            assert wakeup_create["kind"] == "wakeup"
            assert wakeup_create["restored_on_resume"] is False
            assert wakeup_reschedule["schedule_id"] == state["wakeup_id"]
            assert wakeup_reschedule["scheduled_at_ms"] > wakeup_create["scheduled_at_ms"]
            assert state["wakeup_stop"]["stopped"] is True
            assert state["after_wakeup_list"] == []
            assert operations.count("create") == 3, operations
            assert operations.count("delete") == 2, operations
            assert operations.count("reschedule") == 1, operations
            assert operations.count("dispatch") == 1, operations

            tool_request = next(
                request
                for request in requests
                if any(
                    marker in str(message.get("content", ""))
                    for message in request.get("messages", [])
                    for marker in MARKERS
                )
            )
            schemas = {
                entry["function"]["name"]: entry["function"]["parameters"]
                for entry in tool_request["tools"]
                if "function" in entry
            }
            assert {
                "schedule_create",
                "schedule_list",
                "schedule_delete",
                "schedule_wakeup",
            } <= schemas.keys()
            create_properties = schemas["schedule_create"]["properties"]
            assert create_properties["wakeup_seconds"]["minimum"] == 60
            assert create_properties["wakeup_seconds"]["maximum"] == 3600
            assert create_properties["recurring"]["type"] == "boolean"
            assert create_properties["cron"]["type"] == "string"
            wakeup_properties = schemas["schedule_wakeup"]["properties"]
            assert wakeup_properties["delay_seconds"]["minimum"] == 60
            assert wakeup_properties["delay_seconds"]["maximum"] == 3600
            assert wakeup_properties["stop"]["type"] == "boolean"

            (output / "events.json").write_text(json.dumps(events, indent=2))
            (output / "state.json").write_text(json.dumps(state, indent=2))
            manifest = {
                "binary": str(binary),
                "binary_sha256": sha256(binary),
                "binary_mtime_ns": binary.stat().st_mtime_ns,
                "source_sha256": {
                    "orchestration.rs": sha256(
                        Path("crates/heycode-session/src/orchestration.rs")
                    ),
                    "durable_schedule.rs": sha256(
                        Path("crates/heycode-agent/src/durable_schedule.rs")
                    ),
                },
                "semantic_freshness_probes": [
                    "schedule_wakeup published",
                    "wakeup_seconds schema published",
                    "zero cron step rejected",
                    "session_local response compatibility preserved",
                ],
            }
            (output / "manifest.json").write_text(json.dumps(manifest, indent=2))
            result = {
                "status": "passed",
                "runtime": "real heycode CLI TUI over PTY",
                "transport": "loopback HTTP OpenRouter fixture",
                "external_requests": 0,
                "provider_requests": len(requests),
                "captures": captures,
                "cron_restored_once": True,
                "cron_delete_survived_restart": True,
                "one_shot": {
                    "schedule_id": one_shot_id,
                    "inbox_message_id": one_shot_message["id"],
                    "enqueue_seq": one_shot_enqueues[0][0],
                    "dispatch_count": len(one_shot_dispatches),
                    "response_count": state["one_shot_delivery_count"],
                    "not_replayed_after_restart": True,
                },
                "wakeup_rescheduled_and_stopped": True,
                "attributed_reminder_display_verified": True,
                "stopped_wakeup_not_restored": True,
                "invalid_zero_step_rejected": True,
                "schedule_operations": operations,
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result, indent=2))
    finally:
        stop()
        (output / "terminal.ansi").write_bytes(transcript)
        (output / "requests.json").write_text(json.dumps(requests, indent=2))
        if handler_failures:
            (output / "handler-failures.txt").write_text("\n".join(handler_failures))
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=2)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary", type=Path, default=Path("tmp/cli-snapshots/ec95c4bbc96501ca/dshx")
    )
    parser.add_argument(
        "--output", type=Path, default=Path("tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24")
    )
    arguments = parser.parse_args()
    run(arguments.binary, arguments.output)
