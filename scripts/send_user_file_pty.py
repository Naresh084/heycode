#!/usr/bin/env python3
"""Exercise SendUserFile through the production CLI and durable ATT01 path.

Inference is a deterministic OpenRouter-shaped localhost SSE fixture. The run
proves exact catalog/schema exposure, approval, ordered local attachment
admission, rich-result settlement, bounded failure without a false delivery,
and provider-free replay. It never invokes a remote delivery service.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import re
import shutil
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Fixture: SendUserFile",
    "created": 1787752741,
    "description": "Deterministic localhost-only validation fixture",
    "context_length": 131072,
    "architecture": {
        "input_modalities": ["text"],
        "output_modalities": ["text"],
        "tokenizer": "Other",
        "instruct_type": None,
    },
    "pricing": {"prompt": "0", "completion": "0"},
    "top_provider": {
        "context_length": 131072,
        "max_completion_tokens": 32768,
        "is_moderated": False,
    },
    "supported_parameters": [
        "max_tokens",
        "temperature",
        "tool_choice",
        "tools",
        "reasoning",
    ],
    "reasoning": {
        "mandatory": True,
        "default_enabled": True,
        "supported_efforts": ["max", "high", "low"],
        "default_effort": "max",
    },
}

SUCCESS = "SEND_USER_FILE_PTY_SUCCESS"
FAILURE = "SEND_USER_FILE_PTY_FAILURE"


class Screen(pyte.Screen):
    """Reset pyte when the production TUI enters its alternate screen."""

    def set_mode(self, *modes: int, **kwargs: Any) -> Any:
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def tool_call(arguments: dict[str, object], request_index: int) -> dict[str, object]:
    return {
        "index": 0,
        "id": f"send-user-file-{request_index}",
        "type": "function",
        "function": {"name": "SendUserFile", "arguments": json.dumps(arguments)},
    }


def after_last_user(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    start = 0
    for index, message in enumerate(messages):
        if message.get("role") == "user":
            start = index + 1
    return messages[start:]


def run(
    binary: Path,
    output: Path,
    *,
    theme: str,
    color: bool,
    columns: int,
    rows: int,
) -> dict[str, object]:
    binary = binary.resolve()
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, Any]] = []
    request_lock = threading.Lock()
    workspace_ref: dict[str, Path] = {}
    expected_ids: dict[str, str] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond_json(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802
            if self.path.endswith("/key"):
                self.respond_json({"data": {"label": "send-user-file-loopback"}})
            elif "/model/" in self.path:
                self.respond_json({"data": MODEL})
            else:
                self.respond_json(
                    {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                )

        def do_POST(self) -> None:  # noqa: N802
            length = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(length))
            with request_lock:
                requests.append(request)
                request_index = len(requests)
                (output / "requests.json").write_text(
                    json.dumps(requests, indent=2, ensure_ascii=False)
                )

            messages = request.get("messages", [])
            assert isinstance(messages, list)
            users = [
                str(message.get("content", ""))
                for message in messages
                if message.get("role") == "user"
            ]
            last_user = users[-1] if users else ""
            tail = after_last_user(messages)
            tool_results = [message for message in tail if message.get("role") == "tool"]
            tools = request.get("tools", [])

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta: dict[str, object], finish: str | None = None) -> None:
                if "tool_calls" in delta:
                    delta["reasoning"] = (
                        "Preparing only the explicitly requested local conversation files."
                    )
                payload = {
                    "id": f"send-user-file-pty-{request_index}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {"index": 0, "delta": delta, "finish_reason": finish}
                    ],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                self.wfile.flush()

            try:
                if SUCCESS in last_user:
                    if not tool_results:
                        names = [
                            str(tool.get("function", {}).get("name", ""))
                            for tool in tools
                        ]
                        assert "SendUserFile" in names, names
                        tool = next(
                            tool
                            for tool in tools
                            if tool.get("function", {}).get("name") == "SendUserFile"
                        )
                        schema = tool["function"]["parameters"]
                        assert schema["required"] == ["files", "status"], schema
                        assert schema["properties"]["files"]["maxItems"] == 8
                        assert schema["properties"]["status"]["enum"] == [
                            "normal",
                            "proactive",
                        ]
                        assert schema["properties"]["display"]["enum"] == [
                            "render",
                            "attach",
                        ]
                        assert schema["additionalProperties"] is False
                        chunk(
                            {
                                "tool_calls": [
                                    tool_call(
                                        {
                                            "files": [
                                                "deliver/report.txt",
                                                "deliver/opaque.bin",
                                            ],
                                            "caption": "Requested local outputs",
                                            "status": "proactive",
                                            "display": "render",
                                        },
                                        request_index,
                                    )
                                ]
                            }
                        )
                        chunk({}, "tool_calls")
                    else:
                        content = str(tool_results[-1].get("content", ""))
                        assert "No remote or mobile delivery occurred" in content, content
                        assert "heycode://local-user-file/1" in content, content
                        assert "heycode://local-user-file/2" in content, content
                        assert '"local_only": true' in content, content
                        assert '"remote_sent": false' in content, content
                        assert str(workspace_ref["path"]) not in content, content
                        for content_id in expected_ids.values():
                            assert content_id in content, (content_id, content)
                        chunk({"content": "SEND_USER_FILE_PTY_COMPLETE"})
                        chunk({}, "stop")
                elif FAILURE in last_user:
                    if not tool_results:
                        chunk(
                            {
                                "tool_calls": [
                                    tool_call(
                                        {
                                            "files": ["deliver/unsupported.bmp"],
                                            "status": "normal",
                                            "display": "attach",
                                        },
                                        request_index,
                                    )
                                ]
                            }
                        )
                        chunk({}, "tool_calls")
                    else:
                        content = str(tool_results[-1].get("content", ""))
                        assert "tool error:" in content, content
                        assert "rich result could not be committed" in content, content
                        chunk({"content": "SEND_USER_FILE_PTY_REFUSED"})
                        chunk({}, "stop")
                else:
                    chunk({"content": "SEND_USER_FILE_FIXTURE_READY"})
                    chunk({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    os.environ["HEYCODE_SEND_USER_FILE_FIXTURE"] = "local-only-not-a-real-credential"
    captures: list[str] = []
    assertions: list[str] = []

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-send-user-file-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            exports = root / "exports"
            home.mkdir()
            exports.mkdir()
            os.environ["TMPDIR"] = str(exports)
            (workspace / "deliver").mkdir(parents=True)
            workspace_ref["path"] = workspace
            report = b"local report\n"
            opaque = bytes([0, 1, 2, 3, 255])
            (workspace / "deliver/report.txt").write_bytes(report)
            (workspace / "deliver/opaque.bin").write_bytes(opaque)
            (workspace / "deliver/unsupported.bmp").write_bytes(b"BMunsupported")
            expected_ids.update(
                report="sha256-" + hashlib.sha256(report).hexdigest(),
                opaque="sha256-" + hashlib.sha256(opaque).hexdigest(),
            )
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "settings.toml").write_text(
                f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n'
            )
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_SEND_USER_FILE_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            extra = [
                "--provider",
                "openrouter",
                "--model",
                MODEL["id"],
                "--approval",
                "default",
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_SEND_USER_FILE_FIXTURE",
            ]

            def launch(
                resume: Path | None = None,
            ) -> tuple[FullScreenTui, Screen, TerminalByteStream]:
                tui = FullScreenTui(
                    str(home),
                    str(workspace),
                    str(binary),
                    fake=False,
                    rows=rows,
                    columns=columns,
                    color=color,
                    extra=extra + (["--resume", str(resume)] if resume else []),
                )
                screen = Screen(columns, rows)
                return tui, screen, TerminalByteStream(screen)

            tui, screen, stream = launch()

            def read(seconds: float = 0.12) -> str:
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def send(data: bytes) -> None:
                os.write(tui.fd, data)
                read(0.12)

            def wait_for(needle: str, timeout: float = 45) -> str:
                deadline = time.monotonic() + timeout
                current = ""
                while time.monotonic() < deadline:
                    current = read()
                    if needle in current:
                        return current
                    if not tui.alive():
                        raise AssertionError(
                            f"CLI exited while waiting for {needle!r}:\n{current}"
                        )
                raise AssertionError(f"Missing {needle!r}:\n{current}")

            def capture(name: str) -> str:
                current = read(0.3)
                (output / f"{name}.txt").write_text(current)
                render_screen(
                    screen,
                    output / f"{name}.png",
                    background="#ffffff" if theme == "heycode-light" else "#101014",
                    foreground="#24292f" if theme == "heycode-light" else "#dddddd",
                )
                captures.append(name)
                return current

            def click_text(pattern: str) -> None:
                read(0.2)
                for row, line in enumerate(screen.display):
                    match = re.search(pattern, line)
                    if match:
                        column = match.start() + 1
                        terminal_row = row + 1
                        send(f"\x1b[<0;{column};{terminal_row}M".encode())
                        send(f"\x1b[<0;{column};{terminal_row}m".encode())
                        return
                raise AssertionError(
                    f"No clickable text matching {pattern!r}:\n{read(0.1)}"
                )

            def saved_directories() -> list[Path]:
                return sorted(
                    path for path in exports.glob("heycode-files-*") if path.is_dir()
                )

            def assert_saved(directory: Path) -> None:
                saved = sorted(directory.iterdir())
                assert [path.name for path in saved] == [
                    "1-report.txt",
                    "2-opaque.bin",
                ], saved
                assert saved[0].read_bytes() == report
                assert saved[1].read_bytes() == opaque
                if os.name == "posix":
                    assert directory.stat().st_mode & 0o777 == 0o700
                    assert all(path.stat().st_mode & 0o777 == 0o600 for path in saved)

            def approve_pending(capture_name: str) -> None:
                pending = wait_for("Permission requested")
                deadline = time.monotonic() + 20
                while "SendUserFile" not in pending and time.monotonic() < deadline:
                    pending = read()
                assert "SendUserFile" in pending, pending
                capture(capture_name)
                send(b"y")

            def stop_current() -> bytes:
                nonlocal tui
                send(b"/quit\r")
                deadline = time.monotonic() + 8
                while tui.alive() and time.monotonic() < deadline:
                    read(0.1)
                transcript = tui.transcript
                tui.close()
                return transcript

            try:
                initial = read(2)
                if "Welcome to heycode" in initial:
                    send(b"\x1b[B\x1b[B\r")
                    wait_for("Select a provider")
                    send(b"OpenRouter\r")
                    wait_for("Paste your OpenRouter API key")
                    send(b"local-only-not-a-real-credential\r")
                    wait_for("Choose a model")
                    send(b"\r")
                wait_for("Default")
                capture("00-ready")

                send(f"{SUCCESS}\r".encode())
                approve_pending("01-delivery-pending")
                completed = wait_for("SEND_USER_FILE_PTY_COMPLETE", 60)
                for expected in ["report.txt", "opaque.bin"]:
                    assert expected in completed, completed
                capture("02-delivery-completed")
                assertions.append(
                    "two ordered files render only after rich attachment settlement"
                )

                # The delivery receipt is provenance, not authority. Change the
                # source files before the human saves; exact saved bytes must
                # still come from ATT01 rather than either pathname.
                (workspace / "deliver/report.txt").write_bytes(b"changed source\n")
                (workspace / "deliver/opaque.bin").write_bytes(b"changed opaque")
                click_text(r"Local files \(2 files\)")
                expanded = wait_for("attachment:")
                assert "Source paths are provenance" in expanded, expanded
                capture("03-delivery-expanded")
                send(b"w")
                wait_for("Saved local file to")
                assert expected_ids["report"][:24] in expanded, expanded
                first_exports = set(saved_directories())
                assert len(first_exports) == 1
                assert_saved(next(iter(first_exports)))
                capture("04-delivery-saved-after-source-change")
                assertions.append(
                    "w saves exact original bytes from ATT01 after both source files changed, with private directory/file modes"
                )
                send(b"\x1b")

                send(f"{FAILURE}\r".encode())
                approve_pending("05-invalid-delivery-pending")
                refused = wait_for("SEND_USER_FILE_PTY_REFUSED", 60)
                assert "Local file delivery" in refused and "failed" in refused, refused
                capture("06-invalid-delivery-refused")
                assertions.append(
                    "an ATT01-rejected file settles failed without a delivery card"
                )

                logs = sorted(home.rglob("session.jsonl"))
                assert len(logs) == 1, logs
                session_path = logs[0]
                events = [
                    json.loads(line)
                    for line in session_path.read_text().splitlines()
                    if line.strip()
                ]
                calls = [
                    (index, event)
                    for index, event in enumerate(events)
                    if event.get("kind") == "tool/call"
                    and event.get("data", {}).get("name") == "SendUserFile"
                ]
                assert len(calls) == 2, calls
                success_index, success_call = calls[0]
                failure_index, failure_call = calls[1]
                success_id = success_call["data"]["call_id"]
                failure_id = failure_call["data"]["call_id"]
                success_result_index, success_result = next(
                    (index, event)
                    for index, event in enumerate(events)
                    if event.get("kind") == "tool/rich-result"
                    and event.get("data", {}).get("call_id") == success_id
                )
                failure_result_index, failure_result = next(
                    (index, event)
                    for index, event in enumerate(events)
                    if event.get("kind") == "tool/result"
                    and event.get("data", {}).get("call_id") == failure_id
                )
                assert success_index < success_result_index < failure_index
                admitted = [
                    event["data"]["attachment"]
                    for event in events[success_index + 1 : success_result_index]
                    if event.get("kind") == "attachment/added"
                ]
                assert [item["content_id"] for item in admitted] == [
                    expected_ids["report"],
                    expected_ids["opaque"],
                ], admitted
                assert [item["media_type"] for item in admitted] == [
                    "text/plain",
                    "application/octet-stream",
                ], admitted
                result = success_result["data"]["result"]
                structured = result["structuredContent"]["value"]
                assert structured["local_only"] is True
                assert structured["remote_sent"] is False
                assert structured["caption"] == "Requested local outputs"
                assert [item["path"] for item in structured["files"]] == [
                    "deliver/report.txt",
                    "deliver/opaque.bin",
                ]
                blobs = [block for block in result["blocks"] if block["type"] == "embedded_blob"]
                assert [block["media"]["attachment"]["content_id"] for block in blobs] == [
                    expected_ids["report"],
                    expected_ids["opaque"],
                ]
                assert failure_result["data"]["is_error"] is True
                assert not any(
                    event.get("kind") == "attachment/added"
                    for event in events[failure_index + 1 : failure_result_index]
                )
                shutil.copyfile(session_path, output / "session.jsonl")
                (output / "events.json").write_text(
                    json.dumps(events, indent=2, ensure_ascii=False)
                )
                assertions.append(
                    "ATT01 content ids/media types and RichToolResult order match the exact source bytes"
                )

                requests_before_replay = len(requests)
                (output / "live.ansi").write_bytes(stop_current())

                # Remove both source files. A replayed card and save must still
                # use the immutable attachment objects, with no inference call.
                (workspace / "deliver/report.txt").unlink()
                (workspace / "deliver/opaque.bin").unlink()
                tui, screen, stream = launch(session_path)
                replayed = wait_for("report.txt")
                assert "opaque.bin" in replayed, replayed
                assert len(requests) == requests_before_replay
                capture("07-delivery-replayed-source-absent")
                click_text(r"Local files \(2 files\)")
                wait_for("attachment:")
                send(b"w")
                wait_for("Saved local file to")
                all_exports = set(saved_directories())
                second_exports = all_exports - first_exports
                assert len(all_exports) == 2
                assert len(second_exports) == 1
                assert_saved(next(iter(second_exports)))
                capture("08-replayed-delivery-saved")
                time.sleep(0.5)
                read(0.2)
                assert len(requests) == requests_before_replay
                assertions.append(
                    "the durable delivery card and exact-byte save survive source deletion without an inference request"
                )
                (output / "replay-source-absent.ansi").write_bytes(stop_current())

                # Durable metadata says admission succeeded at delivery time;
                # current availability is checked again on save. Remove one
                # disposable attachment object and prove the replayed card does
                # not fabricate a successful export.
                digest = expected_ids["report"].removeprefix("sha256-")
                objects = [
                    path
                    for path in home.rglob(digest[2:])
                    if path.is_file() and path.parent.name == digest[:2]
                ]
                assert len(objects) == 1, objects
                objects[0].unlink()
                tui, screen, stream = launch(session_path)
                unavailable = wait_for("report.txt")
                assert "Stored in this conversation. No remote delivery." in unavailable
                capture("09-replayed-delivery-object-missing")
                click_text(r"Local files \(2 files\)")
                wait_for("attachment:")
                send(b"w")
                wait_for("Local file save failed")
                assert len(saved_directories()) == 2
                assert len(requests) == requests_before_replay
                capture("10-replayed-delivery-save-failed")
                assertions.append(
                    "retained receipt metadata remains inspectable while a missing attachment object makes current save fail explicitly"
                )
                (output / "replay-object-missing.ansi").write_bytes(stop_current())
            finally:
                if tui.alive():
                    tui.close()

            result = {
                "status": "passed",
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "localhost_inference_requests": len(requests),
                "external_provider_requests": False,
                "remote_delivery_attempts": 0,
                "admitted_attachments": 2,
                "successful_rich_deliveries": 1,
                "refused_deliveries": 1,
                "replay_inference_requests": len(requests) - requests_before_replay,
                "captures": captures,
                "assertions": assertions,
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result, indent=2))
            return result
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--theme", default="heycode-light")
    parser.add_argument("--color", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    args = parser.parse_args()
    run(
        args.binary,
        args.output,
        theme=args.theme,
        color=args.color,
        columns=args.columns,
        rows=args.rows,
    )


if __name__ == "__main__":
    main()
