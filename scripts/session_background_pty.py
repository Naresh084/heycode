#!/usr/bin/env python3
"""Whole-session process/PTY lifecycle regression with a local HTTP provider.

No real home, credentials or paid inference. Every fixture-owned host is stopped
before temporary files are removed. Evidence includes terminal captures, request
counts and durable session logs. Requires pyte; use the existing audit venv.
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
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time

import pyte
from task_console_pty import MODEL, Screen


def encode_frame(request):
    data = json.dumps(request).encode()
    return struct.pack(">I", len(data)) + data


def send_frame(stream, request):
    stream.sendall(encode_frame(request))


def read_frame(stream):
    def exact(count):
        data = b""
        while len(data) < count:
            chunk = stream.recv(count - len(data))
            if not chunk:
                raise EOFError("host disconnected")
            data += chunk
        return data
    length, = struct.unpack(">I", exact(4))
    assert length <= 128 * 1024
    return json.loads(exact(length))


def exchange(path, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(path))
        send_frame(stream, request)
        return read_frame(stream)


def records(home):
    return [json.loads(path.read_text()) for path in (home / "background").glob("*.json")]


def live(home):
    result = []
    for record in records(home):
        try:
            response = exchange(record["socket"], {"op": "status"})
            status = response["status"]
            if status["exit_code"] is None:
                result.append(status)
        except (OSError, EOFError):
            pass
    return result


def wait(predicate, description, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError(f"timed out: {description}")


class Terminal:
    def __init__(self, binary, args, home, workspace):
        self.raw = bytearray()
        self.screen = Screen(100, 35)
        self.stream = pyte.ByteStream(self.screen)
        self.pid, self.fd = pty.fork()
        self.reaped = False
        if self.pid == 0:
            environment = dict(os.environ, HEYCODE_HOME=str(home), TERM="xterm-256color", NO_COLOR="1")
            environment.pop("HEYCODE_SESSION_HOST", None)
            environment.pop("HEYCODE_SESSION_HOST_TOKEN", None)
            os.chdir(workspace)
            os.execve(binary, [str(binary), *args], environment)
        self.resize(35, 100)

    def read(self, seconds=.1):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if not select.select([self.fd], [], [], .03)[0]:
                continue
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            self.raw.extend(chunk)
            self.stream.feed(chunk)
        return "\n".join(self.screen.display)

    def send(self, data):
        os.write(self.fd, data)

    def expect(self, text, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            screen = self.read()
            if text in screen:
                return screen
            if not self.alive():
                raise AssertionError(f"terminal exited while waiting for {text}:\n{screen}")
        raise AssertionError(f"missing {text}:\n{screen}")

    def resize(self, rows, cols):
        self.screen.resize(rows, cols)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def alive(self):
        if self.reaped:
            return False
        try:
            pid, _ = os.waitpid(self.pid, os.WNOHANG)
            self.reaped = pid != 0
        except ChildProcessError:
            self.reaped = True
        return not self.reaped

    def close(self):
        # Only this unreaped direct fixture child is eligible for signalling.
        self.read(.3)
        if self.alive():
            os.kill(self.pid, signal.SIGKILL)
            wait(lambda: not self.alive(), "fixture terminal reaped", 5)
        os.close(self.fd)


def run(binary, output):
    output.mkdir(parents=True, exist_ok=True)
    with binary.open("rb") as executable:
        binary_sha256 = hashlib.file_digest(executable, "sha256").hexdigest()
    requests = []
    slow = threading.Event()
    crashed = threading.Event()
    delay_startup = threading.Event()
    startup_delayed = threading.Event()
    release_startup = threading.Event()
    started = {name: threading.Event() for name in ("BG_SLOW", "BG_CRASH")}
    checks = []
    clients = []
    with tempfile.TemporaryDirectory(prefix="heycode-whole-session-") as directory:
        root = Path(directory)
        home, workspace = root / "home", root / "workspace"
        home.mkdir(); workspace.mkdir()
        marker = workspace / "human-approved.txt"

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if delay_startup.is_set():
                    startup_delayed.set()
                    release_startup.wait(8)
                payload = {"data": {"label": "local-only"}} if self.path.endswith("/key") else ({"data": MODEL} if "/model/" in self.path else {"data": [MODEL], "total_count": 1, "links": {"next": None}})
                data = json.dumps(payload).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                messages = request["messages"]
                user_indices = [i for i, message in enumerate(messages) if message["role"] == "user"]
                last_index = user_indices[-1] if user_indices else -1
                last = str(messages[last_index].get("content", "")) if last_index >= 0 else ""
                tool_result = any(message["role"] == "tool" for message in messages[last_index + 1:])
                self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
                def chunk(delta, finish=None):
                    payload = {"id": f"background-fixture-{len(requests)}", "object": "chat.completion.chunk", "model": MODEL["id"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                    self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode()); self.wfile.flush()
                try:
                    if "BG_APPROVE" in last and not tool_result:
                        marker_name = "stop-approval.txt" if "BG_APPROVE_STOP" in last else "human-approved.txt"
                        chunk({"reasoning": "A human must approve the fixture write.", "tool_calls": [{"index": 0, "id": f"approval-{len(requests)}", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": f"printf approved > {marker_name}"})}}]})
                        chunk({}, "tool_calls")
                    else:
                        gate = next((key for key in started if key in last), None)
                        if gate:
                            chunk({"content": f"{gate}_STARTED"})
                            started[gate].set()
                            (slow if gate == "BG_SLOW" else crashed).wait(60)
                        chunk({"content": " APPROVAL_DONE" if tool_result else (" FOLLOWUP_DONE" if "BG_FOLLOWUP" in last else " SESSION_DONE")})
                        chunk({}, "stop")
                    self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        os.environ["HEYCODE_BACKGROUND_FIXTURE"] = "local-only-fixture-credential"
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_BACKGROUND_FIXTURE"\nbase_url = "{base}"\n')
        args = ["--trust-workspace", "--provider", "openrouter", "--model", MODEL["id"], "--approval", "full_access", "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_BACKGROUND_FIXTURE"]
        def terminal(extra=()):
            term = Terminal(binary, [*args, *extra], home, workspace)
            clients.append(term)
            return term
        def capture(term, name):
            (output / f"{name}.txt").write_text(term.read(.2))
            (output / f"{name}.ansi").write_bytes(term.raw)
        def events(session):
            path = home / "sessions" / session / "session.jsonl"
            return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
        def session_status(session):
            return next((status for status in live(home) if status["session_id"] == session), None)
        def stop_all():
            for status in live(home):
                exchange(status["socket"], {"op": "stop"})
            wait(lambda: not live(home), "all fixture session hosts stopped", 25)

        try:
            term = terminal()
            term.expect("Full access")
            term.send(b"/permissions default\r")
            term.expect("Default (shift+tab")
            parent = wait(lambda: next((status["session_id"] for status in live(home) if status["session_id"]), None), "parent registered")
            term.send(b"BG_SLOW\r")
            term.expect("BG_SLOW_STARTED")
            assert started["BG_SLOW"].is_set() and len(requests) == 1
            term.send(b"BG_FOLLOWUP\t")
            wait(lambda: any(event["kind"] == "agent/inbox/splice" and "BG_FOLLOWUP" in json.dumps(event) for event in events(parent)), "follow-up durably accepted")
            term.send(b"/background\r")
            wait(lambda: (term.read(.05), not term.alive())[1], "terminal detached during active inference")
            assert session_status(parent)["attached"] is False
            assert len(requests) == 1
            checks.append("active turn detaches without cancelling or issuing another request")
            slow.set()
            wait(lambda: len(requests) == 2 and sum(event["kind"] == "turn/end" for event in events(parent)) == 2, "detached owner completes original and accepted follow-up exactly once")
            checks.append("durably accepted follow-up executes exactly once while detached")

            term = terminal(["--resume", parent])
            term.expect("FOLLOWUP_DONE")
            capture(term, "01-reattached-completed-followup")
            other = terminal(["--resume", parent])
            other.read(.8)
            wait(lambda: not other.alive(), "second terminal refused")
            assert "already attached" in bytes(other.raw).decode(errors="replace")
            assert len(requests) == 2
            checks.append("exclusive attachment rejects another terminal without another runtime")

            term.send(b"PRESERVED-DRAFT")
            term.expect("PRESERVED-DRAFT")
            term.send(b"\x1d")
            wait(lambda: (term.read(.05), not term.alive())[1], "host detach shortcut")
            term = terminal(["--resume", parent])
            term.expect("PRESERVED-DRAFT")
            term.resize(29, 83); term.read(.4)
            term.expect("PRESERVED-DRAFT")
            capture(term, "02-preserved-draft-resized")
            term.send(b"\x15BG_APPROVE\r")
            term.expect("human-approved.txt")
            term.read(.3)
            assert not marker.exists()
            approval_requests = len(requests)
            term.send(b"\x1d")
            wait(lambda: (term.read(.05), not term.alive())[1], "detach pending human approval")
            time.sleep(.5)
            assert not marker.exists() and len(requests) == approval_requests
            term = terminal(["--resume", parent])
            term.expect("human-approved.txt")
            capture(term, "03-approval-still-waiting")
            term.send(b"y")
            term.expect("APPROVAL_DONE")
            assert marker.read_text() == "approved"
            checks.append("pending approval remains unanswered while detached and is answered on reattach")
            checks.append("draft and resized terminal presentation survive reconnect")

            before_fork = len(requests)
            delay_startup.set()
            term.send(b"/fork\r")
            wait(startup_delayed.is_set, "fork reaches delayed local catalog startup")
            # Hold composition beyond the ordinary two-second control timeout.
            # The parent's host must still answer status and its health watcher.
            until = time.monotonic() + 3.5
            while time.monotonic() < until:
                parent_state = session_status(parent)
                assert parent_state and parent_state["attached"], "slow fork startup made the parent owner unreachable or detached"
                assert term.alive()
                term.read(.1)
            release_startup.set()
            delay_startup.clear()
            term.expect("Parent remains active")
            fork = wait(lambda: next((status["session_id"] for status in live(home) if status["session_id"] and status["session_id"] != parent), None), "background fork registered")
            assert session_status(parent)["attached"] and not session_status(fork)["attached"]
            assert len(requests) == before_fork
            term.send(f"/resume {fork}\r".encode())
            wait(lambda: (term.read(.05), session_status(fork)["attached"] and not session_status(parent)["attached"])[1], "terminal handed to existing fork owner")
            term.expect("Default (shift+tab")
            term.send(f"/stop session {parent}\r".encode())
            wait(lambda: session_status(parent) is None, "parent session gracefully stopped")
            checks.append("fork has distinct writer; parent remains alive until explicit session stop")
            checks.append("slash resume moves terminal attachment without reopening a live session")
            checks.append("background fork preserves current permission policy over stale launch overrides")
            checks.append("slow fork composition keeps parent control and health checks responsive")

            # Crash the fixture's identified broker, never a PID loaded from an
            # old registry. The owned TUI detects IPC loss and releases its lease.
            term.send(b"BG_CRASH\r")
            term.expect("BG_CRASH_STARTED")
            host = session_status(fork)
            exact_argument = str(home / "background" / f'{host["host_id"]}.launch')
            process_rows = subprocess.check_output(["ps", "-Ao", "pid,command"], text=True).splitlines()
            matching = [int(row.strip().split(None, 1)[0]) for row in process_rows if "__session-host " + exact_argument in row]
            assert len(matching) == 1, matching
            os.kill(matching[0], signal.SIGKILL)
            wait(lambda: (term.read(.05), not term.alive())[1], "attached client detects dead broker")
            prior_requests = len(requests)
            time.sleep(.7)
            assert len(requests) == prior_requests
            crashed.set()
            # Explicit resume has to reacquire the durable log's existing writer
            # lease; a leftover child would make this fail, never double-infer.
            term = terminal(["--resume", fork, "--approval", "default"])
            term.expect("Default (shift+tab")
            term.send(b"RECOVERED_PROMPT\r")
            term.expect("SESSION_DONE")
            wait(lambda: len(requests) == prior_requests + 1, "one explicit post-crash request")
            capture(term, "04-explicit-crash-recovery")
            checks.append("host death does not auto-replay inference; explicit recovery reacquires writer lease")

            current = session_status(fork)
            term.send(b"\x1d")
            wait(lambda: (term.read(.05), not term.alive())[1], "detach before sequenced input test")
            before_sequence = len(requests)
            with socket.socket(socket.AF_UNIX) as raw_client:
                raw_client.settimeout(3)
                raw_client.connect(current["socket"])
                send_frame(raw_client, {"op": "attach", "rows": 35, "cols": 100})
                assert read_frame(raw_client)["event"] == "ok"
                message = {"op": "input", "sequence": 1, "bytes": list(b"IPC_EXACTLY_ONCE\r")}
                send_frame(raw_client, message)
                def receive_event(expected):
                    while True:
                        response = read_frame(raw_client)
                        if response["event"] == expected:
                            return response
                assert receive_event("input_accepted")["sequence"] == 1
                send_frame(raw_client, message)
                assert "sequence" in receive_event("error")["message"]
                wait(lambda: len(requests) == before_sequence + 1, "one sequenced model input")
                time.sleep(.3)
                assert len(requests) == before_sequence + 1
                raw_client.sendall(encode_frame({"op": "detach"}) + encode_frame({"op": "input", "sequence": 2, "bytes": list(b"AFTER_DETACH_MUST_NOT_RUN\r")}))
                receive_event("detached")
                time.sleep(.2)
                assert len(requests) == before_sequence + 1
            checks.append("duplicate input sequence is refused before a second provider request")
            term = terminal(["--resume", fork])
            term.expect("Default (shift+tab")
            term.send(b"BG_APPROVE_STOP\r")
            term.expect("stop-approval.txt")
            assert not (workspace / "stop-approval.txt").exists()
            environment = dict(os.environ, HEYCODE_HOME=str(home))
            stop = subprocess.run([str(binary), "sessions", "stop", fork], env=environment, cwd=workspace, capture_output=True, text=True, timeout=20)
            assert stop.returncode == 0, stop.stdout + stop.stderr
            assert not (workspace / "stop-approval.txt").exists()
            wait(lambda: not Path(current["socket"]).exists(), "stopped broker socket removed")
            checks.append("CLI graceful stop cancels pending approval, confirms child exit and removes IPC endpoint")
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            shutil.copytree(home / "sessions", output / "sessions", dirs_exist_ok=True)
            (output / "host-records.json").write_text(json.dumps(records(home), indent=2))
        except BaseException:
            (output / "failed-requests.json").write_text(json.dumps(requests, indent=2))
            (output / "failed-host-records.json").write_text(json.dumps(records(home), indent=2))
            if (home / "sessions").exists():
                shutil.copytree(home / "sessions", output / "failed-sessions", dirs_exist_ok=True)
            for index, client in enumerate(clients):
                capture(client, f"failed-terminal-{index}")
            raise
        finally:
            slow.set(); crashed.set(); release_startup.set()
            stop_all()
            for client in clients:
                client.close()
            server.shutdown(); server.server_close()
            # A deliberately killed host leaves a stale endpoint; remove only
            # these exact fixture records after live ownership is gone.
            for record in records(home):
                Path(record["socket"]).unlink(missing_ok=True)
        (output / "evidence.json").write_text(json.dumps({"binary": str(binary), "binary_sha256": binary_sha256, "checks": checks, "provider_requests": len(requests), "live_hosts_after_cleanup": len(live(home)), "scope": "controlled local HTTP + real processes/PTY; no live provider or Claude visual parity claim"}, indent=2))
    return checks


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--out", type=Path, default=Path("tmp/terminal-evidence/session-background"))
    options = parser.parse_args()
    checks = run(options.binary.resolve(), options.out.resolve())
    print(json.dumps({"passed": len(checks), "checks": checks}, indent=2))
