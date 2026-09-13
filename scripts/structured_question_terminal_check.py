#!/usr/bin/env python3
"""Deterministic structured required/optional question and native child ownership PTY journey.

Runs production CLI and native jobs against a localhost SSE fixture. Provider
behavior is scripted; no subscription, remote inference, or paid call is used.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import re
import subprocess
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import pyte

from agent_navigation_terminal_check import MODEL, Screen
from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui

AGENTS = ("Atlas",)


def run(binary: Path, output: Path, columns: int, rows: int, background: bool, theme: str, color: bool):
    output.mkdir(parents=True, exist_ok=True)
    (output / "harness.py").write_bytes(Path(__file__).read_bytes())
    started = datetime.now(timezone.utc).isoformat()
    requests = []
    release = {name: threading.Event() for name in AGENTS}
    optional_requested = threading.Event()
    optional_release = threading.Event()
    live = {name: threading.Event() for name in AGENTS}
    initial_request = threading.Event()
    initial_release = threading.Event()
    summary_started = threading.Event()
    summary_release = threading.Event()
    snapshots = []
    actions = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            value = {"data": {"label": "local-fixture"}} if self.path.endswith("/key") else ({"data": MODEL} if "/model/" in self.path else {"data": [MODEL], "total_count": 1, "links": {"next": None}})
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            request_number = len(requests)
            messages = request.get("messages", [])
            users = [str(message.get("content", "")) for message in messages if message.get("role") == "user"]
            joined = "\n".join(users)
            child = next((name for name in AGENTS if f"CHILD_EVIDENCE_{name.upper()}" in joined), None)
            calls = [call for message in messages for call in message.get("tool_calls", [])]
            names = [call.get("function", {}).get("name") for call in calls]
            completed = {re.match(r"\[job ([^ ]+) completed\]", text).group(1) for text in users if re.match(r"\[job ([^ ]+) completed\]", text)}
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta, finish=None):
                if "tool_calls" in delta:
                    delta["reasoning"] = "Executing the deterministic interaction fixture."
                value = {"id": f"interaction-{request_number}", "object": "chat.completion.chunk", "model": MODEL["id"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(value) + "\n\n").encode())
                self.wfile.flush()

            def tool(name, arguments, index=0, identity=None):
                return {"index": index, "id": identity or f"fixture-{name}-{request_number}-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}

            def dispatch(batch):
                chunk({"tool_calls": batch})
                chunk({}, "tool_calls")

            def say(text):
                chunk({"content": text})
                chunk({}, "stop")

            try:
                def question(identity, prompt, mode, labels=()):
                    return {"id": identity, "question": prompt, "mode": mode, "options": [{"label": label, "description": f"Fixture explanation for {label}"} for label in labels]}

                if child:
                    if "ask_user_question" not in names:
                        dispatch([
                            tool("ask_user_question_async", {"questions": [question("child-scope", "Which child review scope?", "single_choice", ("Child architecture", "Child tests"))]}, identity="child-optional"),
                            tool("ask_user_question", {"questions": [question("child-required", "Which child execution route?", "single_choice", ("Child route A", "Child route B"))]}, index=1, identity="child-required"),
                        ])
                    elif "Answer to optional question" in joined:
                        say("CHILD_ANSWER_RECEIVED: resumed the exact original child.")
                    else:
                        say("CHILD_WAITING: required input arrived; optional choice remains available.")
                elif not calls:
                    dispatch([tool("ask_user_question", {"questions": [
                        question("format", "Choose a report format.", "single_choice", ("Summary", "Detailed")),
                        question("sections", "Choose report sections.", "multiple_choice", ("Architecture", "Tests", "Risks")),
                        question("detail", "Add a report detail.", "free_text"),
                    ]}, identity="required-batch")])
                elif "ask_user_question_async" not in names:
                    optional_requested.set()
                    if not optional_release.wait(60):
                        raise TimeoutError("optional arrival was not released")
                    dispatch([tool("ask_user_question_async", {"questions": [
                        question("optional-scope", "Which optional scope?", "multiple_choice", ("Architecture", "Tests")),
                        question("optional-detail", "Any optional detail?", "free_text"),
                    ]}, identity="optional-batch")])
                elif "START_CHILD" in joined and "agent" not in names:
                    dispatch([tool("agent", {"label": "Atlas", "prompt": "CHILD_EVIDENCE_ATLAS: ask your own required and optional questions, then report.", "provider": "native", "mode": "continuable", "background": True}, identity="spawn-atlas")])
                elif "agent" in names:
                    say("CHILD_STARTED: original child owns its questions.")
                else:
                    say("OPTIONAL_READY: independent work continues.")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception as error:
                (output / "fixture-error.txt").write_text(repr(error))
                raise

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    os.environ["HEYCODE_INTERACTION_FIXTURE_KEY"] = "local-only-not-a-real-credential"
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-interaction-home-") as home, tempfile.TemporaryDirectory(prefix="heycode-interaction-work-") as workspace:
            subprocess.run(["git", "init", "-b", "interaction-evidence", workspace], check=True, capture_output=True)
            Path(workspace, "evidence.txt").write_text("RETAINED_NATIVE_EVIDENCE\n")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            Path(home, "settings.toml").write_text(f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n')
            Path(home, "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_INTERACTION_FIXTURE_KEY"\nbase_url = "{base}"\n')
            tui = FullScreenTui(home, workspace, str(binary), fake=False, rows=rows, columns=columns, color=color, background=background, extra=["--provider", "openrouter", "--model", MODEL["id"], "--approval", "full_access", "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_INTERACTION_FIXTURE_KEY"])
            screen = Screen(columns, rows)
            stream = pyte.ByteStream(screen)

            def read(seconds=0.15):
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def send(data, description):
                actions.append({"action": description, "bytes_hex": data.hex(), "time": time.monotonic()})
                os.write(tui.fd, data)

            def wait(predicate, description, timeout=40):
                deadline = time.monotonic() + timeout
                latest = ""
                while time.monotonic() < deadline:
                    latest = read()
                    if predicate(latest):
                        return latest
                    if not tui.alive():
                        raise AssertionError(f"CLI exited waiting for {description}:\n{latest}")
                raise AssertionError(f"Missing {description}:\n{latest}")

            def has(text, timeout=40):
                return wait(lambda frame: text in frame, text, timeout)

            def capture(name):
                frame = read(0.3)
                (output / f"{name}.txt").write_text(frame)
                (output / f"{name}.ansi").write_bytes(tui.transcript)
                render_screen(screen, output / f"{name}.png", background="#f8f9fb" if theme == "heycode-light" else "#101014", foreground="#20242c" if theme == "heycode-light" else "#e8eaf0")
                snapshots.append(name)
                return frame

            def quiet(frame):
                assert not re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", frame), f"Raw operational identity:\n{frame}"
                for forbidden in ["question_id", "instruction", "after_revision", "budget", 'Agent "', "[job "]:
                    assert forbidden not in frame, f"Operational metadata {forbidden!r}:\n{frame}"

            try:
                initial = read(2)
                if "Welcome to heycode" in initial:
                    send(b"\x1b[B\x1b[B\r", "select provider onboarding")
                    has("Select a provider")
                    send(b"OpenRouter\r", "choose local OpenRouter fixture")
                    has("Paste your OpenRouter API key")
                    send(b"local-only-not-a-real-credential\r", "use local fixture credential")
                    has("Choose a model")
                    send(b"\r", "choose fixture model")
                has("full access on")
                send(b"Run the structured question fixture.\r", "start structured required batch")
                has("Choose a report format.")
                capture("required-single-choice")
                assert "Question 1 of 3" in read(), "batch position must be visible"
                send(b"\x1b[B\r", "choose Detailed")
                has("Choose report sections.")
                capture("required-multiple-choice-empty")
                send(b"\r", "empty multi-submit must stay pending")
                assert "Choose report sections." in read(), "silence or focus cannot choose defaults"
                send(b" \x1b[B ", "toggle Architecture and Tests")
                frame = capture("required-multiple-choice-selected")
                assert "[x] Architecture" in frame and "[x] Tests" in frame, frame
                send(b"\r", "submit explicit selected labels")
                has("Add a report detail.")
                send(b"/custom report detail\r", "answer free text containing slash")
                wait(lambda _: optional_requested.is_set(), "required batch completed")
                send(b"PARENT_DRAFT", "preserve composer while optional question arrives")
                optional_release.set()
                has("OPTIONAL_READY")
                frame = capture("optional-batch-arrival")
                assert "PARENT_DRAFT" in frame, frame
                quiet(frame)
                send(b"\x1bq", "open optional batch")
                has("Which optional scope?")
                send(b" \x1b[B ", "toggle two optional choices")
                capture("optional-multiple-choice-selected")
                send(b"\x1b", "close optional card without dismissing")
                wait(lambda frame: "PARENT_DRAFT" in frame and "Optional question 1 of" not in frame, "composer restored")
                capture("optional-closed-pending")
                send(b"\x1bq", "reopen optional card")
                frame = has("Which optional scope?")
                assert "[x] Architecture" in frame and "[x] Tests" in frame, frame
                send(b"\r", "answer optional selections")
                wait(lambda frame: "Optional question 1 of" not in frame, "first optional answer settled")
                send(b"\x1bq", "open remaining optional question")
                has("Any optional detail?")
                send(b"\x1b[B\r", "explicitly dismiss remaining optional question")
                wait(lambda frame: "Optional question 1 of" not in frame, "optional dismissal settled")
                capture("optional-batch-settled")
                send(b"\x15START_CHILD\r", "start native continuable child")
                has("Which child execution route?", 60)
                capture("child-required-priority")
                send(b"\r", "answer required child route")
                has("CHILD_STARTED", 60)
                has("1 optional question", 60)
                send(b"\x1bq", "open child optional card")
                has("Which child review scope?", 60)
                frame = capture("child-optional-owned-card")
                assert "Atlas" in frame, frame
                send(b"\r", "answer exact child optional scope")
                def child_resumed(_):
                    return any("CHILD_ANSWER_RECEIVED" in path.read_text() for path in Path(home).rglob("session.jsonl"))
                wait(child_resumed, "child follow-up response saved", 60)
                capture("child-answer-resumed")
                events_by_file = {}
                for path in Path(home).rglob("session.jsonl"):
                    events_by_file[str(path.relative_to(home))] = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                parent_events = next(events for events in events_by_file.values() if any(event.get("kind") == "user/message" and "Run the structured question fixture." in json.dumps(event) for event in events))
                child_events = next(events for events in events_by_file.values() if any(event.get("kind") == "user/message" and "CHILD_EVIDENCE_ATLAS" in json.dumps(event) for event in events))
                parent_text = json.dumps(parent_events)
                child_text = json.dumps(child_events)
                for expected in ["required-batch", "Detailed", "/custom report detail", "optional-batch"]:
                    assert expected in parent_text, expected
                answers = []
                for request in requests:
                    for message in request.get("messages", []):
                        if message.get("tool_call_id") == "required-batch":
                            try:
                                value = json.loads(message.get("content", "{}"))
                                if value not in answers:
                                    answers.append(value)
                            except (ValueError, TypeError):
                                pass
                assert answers and answers[0]["answers"] == [
                    {"id": "format", "question": "Choose a report format.", "answer": "Detailed"},
                    {"id": "sections", "question": "Choose report sections.", "answer": ["Architecture", "Tests"]},
                    {"id": "detail", "question": "Add a report detail.", "answer": "/custom report detail"},
                ], answers
                def optional_admissions(events):
                    return [message for event in events if event.get("kind") == "agent/inbox/splice" for message in event.get("data", {}).get("inserted", []) if "optional_question" in json.dumps(message.get("source", {}))]
                parent_answers = optional_admissions(parent_events)
                child_answers = optional_admissions(child_events)
                assert len(parent_answers) == 1, parent_answers
                assert len(child_answers) == 1, child_answers
                assert "Which optional scope?" in json.dumps(parent_answers), parent_answers
                assert "Which child review scope?" in json.dumps(child_answers), child_answers
                assert "Child architecture" in json.dumps(child_answers), child_answers
                assert "CHILD_ANSWER_RECEIVED" in child_text, child_text
                completions = []
                (output / "native-events.json").write_text(json.dumps(events_by_file, indent=2))
                if not color:
                    for encoded in re.findall(rb"\x1b\[([0-9;]*)m", bytes(tui.transcript)):
                        for parameter in encoded.split(b";"):
                            if parameter:
                                number = int(parameter)
                                assert number not in (38, 48) and not (30 <= number <= 37 or 40 <= number <= 47 or 90 <= number <= 107), f"NO_COLOR emitted color SGR: {encoded!r}"
                result = {"harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), "passed": True, "started_utc": started, "finished_utc": datetime.now(timezone.utc).isoformat(), "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "background": background, "viewport": [columns, rows], "theme": theme, "color": color, "screens": snapshots, "parent_optional_answers": len(parent_answers), "child_optional_answers": len(child_answers), "provider_requests": len(requests), "external_provider": False, "transport": "localhost deterministic SSE fixture", "limitations": ["Scripted model behavior; no subscription model tested", "Provider-native vendor SDK dialog adapters are tested separately"]}
                (output / "result.json").write_text(json.dumps(result, indent=2))
                print(json.dumps(result, indent=2))
                return result
            except Exception:
                capture("failure")
                raise
            finally:
                (output / "requests.json").write_text(json.dumps(requests, indent=2))
                (output / "actions.json").write_text(json.dumps(actions, indent=2))
                retained_events = {}
                for path in Path(home).rglob("session.jsonl"):
                    retained_events[str(path.relative_to(home))] = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                (output / "native-events.json").write_text(json.dumps(retained_events, indent=2))
                optional_release.set()
                initial_release.set()
                summary_release.set()
                for event in release.values():
                    event.set()
                # Ask this disposable session to quit before the attachment closes.
                if tui.alive():
                    try:
                        send(b"\x1b", "close inspection before shutdown")
                        read(0.2)
                        send(b"\x15/quit\r", "graceful fixture shutdown")
                        read(1)
                    except OSError:
                        pass
                cleanup = []
                if background:
                    for path in Path(home, "background").glob("*.json"):
                        try:
                            record = json.loads(path.read_text())
                        except (OSError, ValueError):
                            continue
                        if record.get("exit_code") is None and record.get("host_id"):
                            stopped = subprocess.run([str(binary), "sessions", "stop", record["host_id"]], env={**os.environ, "HEYCODE_HOME": home}, capture_output=True, text=True, timeout=15)
                            cleanup.append({"host_id": record["host_id"], "returncode": stopped.returncode, "stdout": stopped.stdout, "stderr": stopped.stderr})
                (output / "cleanup.json").write_text(json.dumps(cleanup, indent=2))
                tui.close()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    parser.add_argument("--background", action="store_true", help="exercise the default session broker")
    parser.add_argument("--theme", choices=["heycode-dark", "heycode-light"], default="heycode-dark")
    parser.add_argument("--color", action="store_true")
    args = parser.parse_args()
    run(args.binary.resolve(), args.output.resolve(), args.columns, args.rows, args.background, args.theme, args.color)
