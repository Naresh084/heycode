#!/usr/bin/env python3
"""Deterministic native optional-question/three-agent/control/completion PTY journey.

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

AGENTS = ("Atlas", "Boreal", "Cygnus")


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
                if child:
                    if "read" not in names:
                        dispatch([tool("read", {"path": "evidence.txt"}, identity=f"evidence-{child.lower()}")])
                    else:
                        chunk({"content": f"{child} is checking the retained evidence."})
                        live[child].set()
                        if not release[child].wait(120):
                            raise TimeoutError(f"child {child} was not released")
                        say(f" RESULT_{child.upper()}: evidence verified.")
                elif not calls:
                    initial_request.set()
                    if not initial_release.wait(30):
                        raise TimeoutError("initial dispatch was not released")
                    batch = [tool("ask_user_question_async", {"question": "Which review scope would you prefer?", "options": ["Architecture", "Tests"]}, identity="scope-question")]
                    batch.extend(tool("agent", {"label": f"{name} Review", "prompt": f"CHILD_EVIDENCE_{name.upper()}: read evidence.txt and report the result.", "provider": "native", "mode": "oneshot"}, index=i + 1, identity=f"spawn-{name.lower()}") for i, name in enumerate(AGENTS))
                    dispatch(batch)
                elif "agent_control" not in names:
                    dispatch([tool("agent_control", {"action": "list"}, identity="inspect-agents"), tool("job_control", {"action": "list"}, index=1, identity="inspect-jobs")])
                elif not any(call.get("id") == "wait-agents" for call in calls):
                    records = []
                    for message in messages:
                        if message.get("tool_call_id") == "inspect-agents":
                            try:
                                value = json.loads(message.get("content", "{}"))
                                records = value.get("agents", [])
                            except (ValueError, TypeError):
                                pass
                    targets = [{"agent_id": row["id"], "after_revision": row["revision"]} for row in records]
                    if not targets:
                        raise AssertionError("canonical agent list did not return native identities")
                    dispatch([tool("agent_control", {"action": "wait", "targets": targets, "timeout_ms": 800}, identity="wait-agents")])
                elif len(completed) >= 3:
                    chunk({"content": "All three reviews are complete. I am combining the findings. "})
                    summary_started.set()
                    if not summary_release.wait(60):
                        raise TimeoutError("summary was not released")
                    say("REVIEW_COMPLETE: Atlas, Boreal, and Cygnus each verified the evidence.")
                elif completed:
                    say("Review results received; the remaining reviews are still running.")
                else:
                    say("REVIEW_READY: The three reviews are running; the optional scope question is available.")
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
                send(b"Review the native interaction fixture.\r", "start review")
                wait(lambda _: initial_request.is_set(), "initial native request")
                send(b"PARENT_DRAFT", "type while optional question arrives")
                initial_release.set()
                has("REVIEW_READY", 60)
                wait(lambda _: all(event.is_set() for event in live.values()), "three native live children")
                frame = capture("question-and-three-agents")
                assert "PARENT_DRAFT" in frame, frame
                assert "3 agents" in frame, frame
                quiet(frame)
                send(b"\x1bq", "open optional question")
                has("Which review scope")
                capture("optional-question-open")
                send(b"\x1b", "close without dismissing")
                frame = wait(lambda frame: "PARENT_DRAFT" in frame and "Optional question 1 of" not in frame, "closed question preserves composer")
                assert "optional question" in frame, frame
                capture("optional-question-closed-pending")
                send(b"\x1bq", "reopen pending question")
                has("Which review scope")
                send(b"\r", "answer Architecture")
                wait(lambda frame: "Optional question 1 of" not in frame and "Alt+Q to answer" not in frame, "question answered and panel closed")
                frame = capture("question-answered-draft-preserved")
                assert "PARENT_DRAFT" in frame, frame
                send(b"\x15", "clear preserved draft")
                release["Atlas"].set()
                has("Review results received", 45)
                quiet(capture("staggered-completion"))
                release["Boreal"].set()
                release["Cygnus"].set()
                wait(lambda _: summary_started.is_set(), "parent combining all results", 60)
                frame = capture("parent-responding-after-completion")
                assert "Responding" in frame or "responding" in frame, frame
                summary_release.set()
                has("REVIEW_COMPLETE", 60)
                wait(lambda frame: "Working" not in frame and "Responding" not in frame, "settled parent and child activity")
                frame = capture("settled-review")
                quiet(frame)
                assert "interaction-evidence" in frame, frame
                assert "ctx" in frame.lower() or "context" in frame.lower(), frame
                assert "❯" in frame or "›" in frame, frame
                send(b"\x0f", "expand exact transcript metadata")
                frame = capture("expanded-retained-history")
                assert "[job " in frame and "completed]" in frame, frame
                # Earlier argument cards remain reachable by scrolling.
                send(b"\x0f", "collapse transcript metadata")
                has("REVIEW_COMPLETE")
                send(b"/agents\r", "explicit inspection of retained agents")
                has("Agents")
                capture("explicit-agent-inspection")
                events_by_file = {}
                for path in Path(home).rglob("session.jsonl"):
                    events_by_file[str(path.relative_to(home))] = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                parent_events = next(events for events in events_by_file.values() if any(event.get("kind") == "user/message" and "Review the native interaction fixture." in json.dumps(event) for event in events))
                completions = [event for event in parent_events if event.get("kind") == "user/message" and "[job " in event.get("data", {}).get("text", "")]
                assert len(completions) == 3, f"expected exactly three admitted completion messages: {completions}"
                for name in AGENTS:
                    assert sum(f"RESULT_{name.upper()}" in json.dumps(event) for event in completions) == 1, name
                assert sum(event.get("kind") == "user/message" and "Architecture" in json.dumps(event) for event in parent_events) == 1, "optional answer must be admitted exactly once"
                retained = json.dumps(parent_events)
                for expected in ["scope-question", "inspect-agents", "inspect-jobs", "wait-agents", "question_id", "instruction"]:
                    assert expected in retained, f"missing exact retained metadata {expected}"
                (output / "native-events.json").write_text(json.dumps(events_by_file, indent=2))
                if not color:
                    for encoded in re.findall(rb"\x1b\[([0-9;]*)m", bytes(tui.transcript)):
                        for parameter in encoded.split(b";"):
                            if parameter:
                                number = int(parameter)
                                assert number not in (38, 48) and not (30 <= number <= 37 or 40 <= number <= 47 or 90 <= number <= 107), f"NO_COLOR emitted color SGR: {encoded!r}"
                result = {"harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), "passed": True, "started_utc": started, "finished_utc": datetime.now(timezone.utc).isoformat(), "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "background": background, "viewport": [columns, rows], "theme": theme, "color": color, "screens": snapshots, "native_completion_messages": len(completions), "provider_requests": len(requests), "external_provider": False, "transport": "localhost deterministic SSE fixture", "limitations": ["Scripted model behavior; no subscription model tested", "No crash-origin claim from this lifecycle journey"]}
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
