#!/usr/bin/env python3
"""Capture the current heycode `/advisor` contract against a loopback provider.

The real CLI runs with a disposable home/workspace and a deterministic
OpenRouter-shaped provider on 127.0.0.1. The request is held long enough to
capture the scheduled turn, then completed without contacting an external
provider or using the operator's credentials.
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
from typing import Any

import pyte

from agent_navigation_terminal_check import MODEL, Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


QUESTION = "choose the safer persistence boundary for the current advisor design"
COMMAND = f"/advisor {QUESTION}"
ANSWER = "LOCAL HEYCODE ADVISOR: keep persisted route selection separate from live turn state."


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(binary: Path, output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, Any]:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    requests: list[dict[str, Any]] = []
    entered = threading.Event()
    release = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def json_response(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            if self.path.endswith("/key"):
                self.json_response({"data": {"label": "local advisor fixture"}})
            elif "/model/" in self.path:
                self.json_response({"data": MODEL})
            else:
                self.json_response({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            record = {
                "method": "POST",
                "path": self.path,
                "loopback": True,
                "request_bytes": len(raw),
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "tool_count": len(body.get("tools", [])),
                "question_present": QUESTION in raw.decode(errors="replace"),
            }
            requests.append(record)
            entered.set()
            if not release.wait(timeout):
                raise TimeoutError("advisor fixture was not released")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for content, finish in ((ANSWER, None), (None, "stop")):
                delta = {} if content is None else {"content": content}
                payload = {
                    "id": "heycode-advisor-current",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    result: dict[str, Any] = {
        "status": "blocked",
        "scope": "current heycode one-shot /advisor with a localhost provider",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
        "normal_conversation_prompts": 0,
        "credential_source": "dummy loopback key only",
    }

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-advisor-current-") as folder:
            root = Path(folder)
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
                'api_key_env = "HEYCODE_ADVISOR_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            os.environ["HEYCODE_ADVISOR_FIXTURE"] = "local-only-dummy"
            tui = FullScreenTui(
                str(home),
                str(workspace),
                str(binary),
                fake=False,
                color=True,
                rows=rows,
                columns=columns,
                extra=[
                    "--provider", "openrouter",
                    "--model", MODEL["id"],
                    "--set", f"llm.base_url={base}",
                    "--set", "llm.api_key_env=HEYCODE_ADVISOR_FIXTURE",
                ],
            )
            screen: pyte.Screen = Screen(columns, rows)
            stream = TerminalByteStream(screen)
            captures: list[str] = []

            def visible() -> str:
                return "\n".join(screen.display)

            def read(seconds: float = 0.15) -> str:
                stream.feed(tui.read(seconds))
                return visible()

            def wait_text(needle: str) -> str:
                deadline = time.monotonic() + timeout
                latest = visible()
                while time.monotonic() < deadline:
                    latest = read()
                    if needle.lower() in latest.lower():
                        return latest
                    if not tui.alive():
                        raise RuntimeError(f"heycode exited while waiting for {needle!r}:\n{latest}")
                raise TimeoutError(f"timed out waiting for {needle!r}:\n{latest}")

            def capture(name: str) -> str:
                text = read(0.35)
                (output / f"{name}.txt").write_text(text)
                render_screen(screen, output / f"{name}.png")
                captures.append(name)
                return text

            try:
                wait_text("shift+tab to cycle")
                capture("00-ready")
                os.write(tui.fd, COMMAND.encode() + b"\r")
                deadline = time.monotonic() + timeout
                while not entered.is_set() and time.monotonic() < deadline:
                    read()
                if not entered.is_set():
                    raise TimeoutError("advisor provider request was not observed")
                held = capture("01-held")
                release.set()
                completed = wait_text(ANSWER)
                capture("02-completed")
                logs = list(home.rglob("session.jsonl"))
                parent_logs = []
                for path in logs:
                    events = [json.loads(line) for line in path.read_text().splitlines() if line]
                    if any(
                        event.get("kind") == "user/message"
                        and event.get("data", {}).get("text") == COMMAND
                        for event in events
                    ):
                        parent_logs.append((path, events))
                if len(parent_logs) != 1:
                    raise AssertionError(f"expected one parent log, found {len(parent_logs)} of {len(logs)}")
                parent_path, parent_events = parent_logs[0]
                (output / "parent-session.jsonl").write_text(parent_path.read_text())
                (output / "requests.json").write_text(json.dumps(requests, indent=2))
                assistants = [
                    event.get("data", {}).get("content", "")
                    for event in parent_events
                    if event.get("kind") == "assistant/message"
                ]
                result.update(
                    {
                        "status": "passed",
                        "captures": captures,
                        "provider_requests": requests,
                        "session_log_count": len(logs),
                        "contract": {
                            "one_shot_command_with_instructions": COMMAND in held or COMMAND in completed,
                            "provider_request_has_question": len(requests) == 1 and requests[0]["question_present"],
                            "inherits_current_route_model": len(requests) == 1 and requests[0]["model"] == MODEL["id"],
                            "fresh_child_has_tools": len(requests) == 1 and requests[0]["tool_count"] > 0,
                            "answer_committed_to_parent": any(ANSWER in text for text in assistants),
                            "durable_child_task_identity": any("[task_id:" in text for text in assistants),
                            "no_persistent_picker_or_toggle": "No advisor" not in completed and "Advisor (experimental)" not in completed,
                        },
                    }
                )
                if not all(result["contract"].values()):
                    result["status"] = "gaps_observed"
            finally:
                release.set()
                (output / "terminal.ansi").write_bytes(tui.transcript)
                tui.close()
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        (output / "result.json").write_text(json.dumps(result, indent=2))

    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=46)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    outcome = run(args.binary, args.output, columns=args.columns, rows=args.rows, timeout=args.timeout)
    raise SystemExit(0 if outcome["status"] == "passed" else 1)
