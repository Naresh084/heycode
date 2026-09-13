#!/usr/bin/env python3
"""Deterministic five-agent message, completion, failure and navigation PTY journey.

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

AGENTS = ("Atlas", "Boreal", "Cygnus", "Draco", "Equinox")
FAILURE = "FIXTURE_PROVIDER_REFUSAL_EQUINOX"
PARENT_NOTE = "PARENT_NOTE_ATLAS: verify the queued message at the next safe step"


def run(binary: Path, output: Path, columns: int, rows: int, background: bool, theme: str, color: bool):
    output.mkdir(parents=True, exist_ok=True)
    started = datetime.now(timezone.utc).isoformat()
    requests = []
    release = {name: threading.Event() for name in AGENTS}
    live = {name: threading.Event() for name in AGENTS}
    initial_request = threading.Event()
    initial_release = threading.Event()
    summary_started = threading.Event()
    summary_release = threading.Event()
    parent_note_received = threading.Event()
    spawn_results = {}
    observed_receipts = set()
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
            completed = {name for name in AGENTS if any(re.match(r"\[agent " + re.escape(name) + r" \([^)]*\) (completed|failed); run ", text) for text in users)}
            if child == "Equinox" and "read" in names:
                data = json.dumps({"error": {"message": FAILURE, "type": "invalid_request_error", "code": 400}}).encode()
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta, finish=None):
                if "tool_calls" in delta:
                    delta["reasoning"] = "Executing the deterministic agent message fixture."
                value = {"id": f"agent-message-{request_number}", "object": "chat.completion.chunk", "model": MODEL["id"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
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
                if child:
                    if "send_message" not in names:
                        dispatch([tool("send_message", {"to": "main", "message": f"CHILD_PROGRESS_{child.upper()}: ready for review"}, identity=f"child-to-main-{child.lower()}")])
                    elif "read" not in names:
                        chunk({"content": f"{child} is reviewing the evidence. "})
                        live[child].set()
                        if not release[child].wait(120):
                            raise TimeoutError(f"child {child} was not released")
                        dispatch([tool("read", {"path": "evidence.txt"}, identity=f"evidence-{child.lower()}")])
                    else:
                        if child == "Atlas":
                            assert PARENT_NOTE in joined, "parent message was not admitted before Atlas's next safe step"
                            parent_note_received.set()
                        say(f"RESULT_{child.upper()}: evidence verified.")
                elif not calls:
                    initial_request.set()
                    if not initial_release.wait(30):
                        raise TimeoutError("initial dispatch was not released")
                    dispatch([tool("agent", {"label": name, "prompt": f"CHILD_EVIDENCE_{name.upper()}: read evidence.txt and report the result.", "provider": "native", "mode": "continuable", "background": True}, index=index, identity=f"spawn-{name.lower()}") for index, name in enumerate(AGENTS)])
                elif not any(call.get("id") == "parent-to-atlas" for call in calls):
                    for message in messages:
                        identity = message.get("tool_call_id", "")
                        if identity.startswith("spawn-"):
                            value = json.loads(message.get("content", "{}"))
                            assert isinstance(value, dict) and value.get("agent_id") and value.get("name"), value
                            assert value.get("delivery") == "automatic", value
                            spawn_results[value["name"]] = value
                    assert set(spawn_results) == set(AGENTS), spawn_results
                    dispatch([tool("send_message", {"to": spawn_results["Atlas"]["agent_id"], "message": PARENT_NOTE}, identity="parent-to-atlas")])
                elif len(completed) == len(AGENTS):
                    chunk({"content": "All five agents have settled. Four succeeded; Equinox needs review. "})
                    summary_started.set()
                    if not summary_release.wait(60):
                        raise TimeoutError("summary was not released")
                    say("MESSAGE_JOURNEY_COMPLETE: four verified results and one retained provider failure.")
                elif completed:
                    say("Partial results received.")
                else:
                    say("MESSAGE_READY: five agents are reviewing; direct messaging is queued.")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception as error:
                (output / "fixture-error.txt").write_text(repr(error))
                raise

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    os.environ["HEYCODE_MESSAGE_FIXTURE_KEY"] = "local-only-not-a-real-credential"
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-message-home-") as home, tempfile.TemporaryDirectory(prefix="heycode-message-work-") as workspace:
            subprocess.run(["git", "init", "-b", "message-evidence", workspace], check=True, capture_output=True)
            Path(workspace, "evidence.txt").write_text("RETAINED_NATIVE_EVIDENCE\n")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            Path(home, "settings.toml").write_text(f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n')
            Path(home, "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_MESSAGE_FIXTURE_KEY"\nbase_url = "{base}"\n')
            tui = FullScreenTui(home, workspace, str(binary), fake=False, rows=rows, columns=columns, color=color, background=background, extra=["--provider", "openrouter", "--model", MODEL["id"], "--approval", "full_access", "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_MESSAGE_FIXTURE_KEY"])
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
                send(b"Review the native message fixture.\r", "start five-agent review")
                wait(lambda _: initial_request.is_set(), "initial native request")
                initial_release.set()
                has("MESSAGE_READY", 60)
                wait(lambda _: all(event.is_set() for event in live.values()), "five native live agents")
                frame = capture("five-agents-running")
                assert "5 agents" in frame, frame
                quiet(frame)

                # Each key is separated by several refresh ticks. Merely moving
                # focus must not switch the conversation or fill an agent ring.
                send(b"\x1b[B", "focus Main in active-agent selector")
                read(0.4)
                for index in range(5):
                    send(b"\x1b[B", f"move down through active agent {index + 1}")
                    frame = read(0.4)
                    assert "● main" in frame, frame
                send(b"\r", "open keyboard-focused fifth agent")
                frame = wait(lambda frame: "○ main" in frame and any(f"● {name}" in frame for name in AGENTS), "deliberate child foreground")
                send(b"CHILD_DRAFT", "preserve unsent child draft")
                capture("child-foreground-draft")

                def click_main():
                    read(0.2)
                    for y, line in enumerate(screen.display):
                        match = re.match(r"[○●] main\s", line)
                        if match:
                            send(f"\x1b[<0;1;{y+1}M\x1b[<0;1;{y+1}m".encode(), "return Main using rendered hit region")
                            return
                    raise AssertionError("Main row unavailable")

                click_main()
                frame = wait(lambda frame: "● main" in frame and "CHILD_DRAFT" not in frame, "main return without expanded history")
                assert "Background tasks" not in frame, frame
                send(b"PARENT_DRAFT", "preserve unsent parent draft during settlement")
                capture("main-draft-restored")
                release["Atlas"].set()
                wait(lambda _: parent_note_received.is_set(), "Atlas consumes direct parent message")
                has("○ Atlas completed", 60)
                observed_receipts.add("Atlas")
                capture("staggered-agent-completion")
                for name in AGENTS[1:]:
                    release[name].set()
                wait(lambda _: summary_started.is_set(), "parent receives five automatic settlements", 60)
                frame = capture("parent-synthesis-after-settlement")
                assert "Responding" in frame or "responding" in frame, frame
                summary_release.set()
                has("MESSAGE_JOURNEY_COMPLETE", 60)
                frame = wait(lambda frame: "Working" not in frame and "Responding" not in frame, "parent fully settled")
                frame = capture("settled-agent-messages")
                quiet(frame)
                assert "PARENT_DRAFT" in frame, frame
                assert not re.search(r"\b[1-5] agents\b", frame), frame
                assert "message-evidence" in frame, frame
                assert "ctx" in frame.lower() or "context" in frame.lower(), frame
                for name in AGENTS[:-1]:
                    if f"○ {name} completed" in frame:
                        assert frame.count(f"○ {name} completed") == 1, frame
                        observed_receipts.add(name)
                assert observed_receipts == set(AGENTS[:-1]), f"Missing visible receipts: {observed_receipts}\n{frame}"
                assert "Equinox" in frame and ("failed" in frame.lower() or "issue" in frame.lower()), frame

                send(b"\x15/agents\r", "open retained agent history explicitly")
                has("Agents")
                capture("retained-agent-history")
                # Navigate every retained history row by keyboard and inspect
                # each until the authoritative failure is visible.
                found_failure = False
                for index in range(5):
                    send(b"\r", f"inspect retained agent {index + 1}")
                    frame = read(0.4)
                    if FAILURE in re.sub(r"\s+", "", frame):
                        found_failure = True
                        settled_detail = capture("provider-failure-detail")
                        assert read(1.2) == settled_detail, "Settled failure detail or duration changed without a new event"
                        break
                    send(b"\x1b", "return to same agent-history origin")
                    read(0.2)
                    send(b"\x1b[B", "next retained agent")
                    read(0.2)
                assert found_failure, "Provider failure must be readable through keyboard history inspection"

                events_by_file = {}
                for path in Path(home).rglob("session.jsonl"):
                    events_by_file[str(path.relative_to(home))] = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                for path, events in events_by_file.items():
                    turns = [event for event in events if event.get("kind") in {"turn/start", "turn/end"}]
                    if turns:
                        assert turns[-1]["kind"] == "turn/end", f"Owner still active: {path}"
                        assert sum(event["kind"] == "turn/start" for event in turns) == sum(event["kind"] == "turn/end" for event in turns), f"Unsettled owner turn: {path}"
                parent_events = next(events for events in events_by_file.values() if any(event.get("kind") == "user/message" and "Review the native message fixture." in json.dumps(event) for event in events))
                attributed = [message for event in parent_events if event.get("kind") == "agent/inbox/splice" for message in event.get("data", {}).get("inserted", []) if message.get("source", {}).get("kind") == "agent"]
                completions = [message for message in attributed if message["source"].get("completion_id")]
                assert len(completions) == 5, completions
                assert len({message["source"]["completion_id"] for message in completions}) == 5, completions
                assert {message["source"]["agent_name"] for message in completions} == set(AGENTS), completions
                assert sum(message["source"]["outcome"] == "completed" for message in completions) == 4, completions
                assert sum(message["source"]["outcome"] == "failed" for message in completions) == 1, completions
                admitted = [event for event in parent_events if event.get("kind") == "user/message"]
                for name in AGENTS:
                    needle = f"[agent {name} ("
                    assert sum(needle in event.get("data", {}).get("text", "") for event in admitted) == 1, name
                    assert sum(f"CHILD_PROGRESS_{name.upper()}" in event.get("data", {}).get("text", "") for event in admitted) == 1, name
                all_calls = {call["id"]: call["function"] for request in requests for message in request.get("messages", []) for call in message.get("tool_calls", [])}
                forbidden_calls = [call for call in all_calls.values() if call.get("name") in {"list_agents", "list_jobs", "list_tasks", "agent_control", "job_control", "interrupt_task"}]
                assert not forbidden_calls, forbidden_calls
                assert sum(call.get("name") == "send_message" for call in all_calls.values()) == 6, all_calls
                (output / "native-events.json").write_text(json.dumps(events_by_file, indent=2))
                (output / "spawn-results.json").write_text(json.dumps(spawn_results, indent=2))
                if not color:
                    for encoded in re.findall(rb"\x1b\[([0-9;]*)m", bytes(tui.transcript)):
                        for parameter in encoded.split(b";"):
                            if parameter:
                                number = int(parameter)
                                assert number not in (38, 48) and not (30 <= number <= 37 or 40 <= number <= 47 or 90 <= number <= 107), f"NO_COLOR emitted color SGR: {encoded!r}"
                result = {"harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), "passed": True, "started_utc": started, "finished_utc": datetime.now(timezone.utc).isoformat(), "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "background": background, "viewport": [columns, rows], "theme": theme, "color": color, "screens": snapshots, "native_completion_messages": len(completions), "direct_message_calls": 6, "inspection_or_wait_calls": 0, "failed_runs": 1, "visible_success_receipts": sorted(observed_receipts), "provider_requests": len(requests), "external_provider": False, "transport": "localhost deterministic SSE fixture", "limitations": ["Scripted model behavior; no subscription model tested", "No live-provider, peer-message, nested delegation, restart, or cancellation claim from this fixture"]}
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
