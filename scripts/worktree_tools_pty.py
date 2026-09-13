#!/usr/bin/env python3
"""Actual worktree tool cards with disposable Git state and localhost inference.

This is controlled UI/execution evidence, not a live commercial-model pilot.
Only enter, nested-enter refusal, retained exit and inactive-exit refusal run.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

from workspace_transitions_pty import Terminal, MODEL
from terminal_screenshot import TerminalByteStream


MARKERS = ("LOCAL_WORKTREE_ENTER", "LOCAL_WORKTREE_NESTED", "LOCAL_WORKTREE_EXIT", "LOCAL_WORKTREE_EXIT_AGAIN")


def text_blocks(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "\n".join(text_blocks(item) for item in value)
    if isinstance(value, dict):
        return str(value.get("text", "")) + "\n" + text_blocks(value.get("content", []))
    return ""


def run(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    requests, other_requests, tool_results = [], [], []
    model_errors = []
    request_lock = threading.Lock()
    expected_names = ("enter_worktree", "exit_worktree") if args.engine == "heycode" else ("EnterWorktree", "ExitWorktree")

    class Handler(http.server.BaseHTTPRequestHandler):
        def json_response(self, body, status=200):
            encoded = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_CONNECT(self):
            other_requests.append({"method": self.command, "path": self.path})
            self.json_response({"error": "external transport disabled"}, 503)

        def do_GET(self):
            other_requests.append({"method": self.command, "path": self.path})
            if args.engine != "heycode":
                self.json_response({"error": "no external lookup"}, 503)
            elif self.path.endswith("/key"):
                self.json_response({"data": {"label": "isolated-worktree-fixture"}})
            elif "/model/" in self.path:
                self.json_response({"data": MODEL})
            else:
                self.json_response({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if length > 2 * 1024 * 1024:
                    raise RuntimeError("request body exceeds fixture bound")
                request = json.loads(self.rfile.read(length))
                if "count_tokens" in self.path:
                    other_requests.append({"method": self.command, "path": self.path})
                    self.json_response({"input_tokens": 0})
                    return
                messages = request.get("messages", [])
                user_text = "\n".join(text_blocks(m.get("content", "")) for m in messages if m.get("role") == "user")
                occurrences = [(user_text.rfind(marker + ":"), marker) for marker in MARKERS]
                position, marker = max(occurrences)
                if position < 0:
                    raise RuntimeError("unexpected request without an authorized fixture marker")
                with request_lock:
                    if len(requests) >= 12:
                        raise RuntimeError("fixture model request budget exceeded")
                    requests.append({"path": self.path, "body": request})
                    (output / "requests.json").write_text(json.dumps(requests, indent=2))
                tools = request.get("tools", [])
                names = [tool.get("function", tool).get("name") for tool in tools]
                if not all(name in names for name in expected_names):
                    raise RuntimeError(f"requested worktree tools are not both advertised: {names}")
                call_id = "call-" + marker.lower()
                if args.engine == "heycode":
                    matching = [m for m in messages if m.get("role") == "tool" and m.get("tool_call_id") == call_id]
                else:
                    matching = [block for m in messages for block in (m.get("content", []) if isinstance(m.get("content"), list) else [])
                                if block.get("type") == "tool_result" and block.get("tool_use_id") == call_id]
                if matching:
                    result = matching[-1]
                    tool_results.append({"marker": marker, "result": result})
                    body = text_blocks(result.get("content", ""))
                    if marker in (MARKERS[1], MARKERS[3]):
                        if not any(word in body.lower() for word in ("already", "no active", "no-op", "not in", "not inside", "not currently")):
                            raise RuntimeError(f"expected actionable worktree refusal: {body}")
                    elif any(word in body.lower() for word in ("tool error", "error:", "could not", "failed", '"error"')):
                        raise RuntimeError(f"expected successful worktree operation: {body}")
                    tool = None
                    arguments = {}
                else:
                    entering = marker in (MARKERS[0], MARKERS[1])
                    tool = expected_names[0 if entering else 1]
                    arguments = {} if args.engine == "heycode" else ({"name": "bounded-reference"} if entering else {"action": "keep"})
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if args.engine == "heycode":
                    delta = {"reasoning": "Controlled local fixture exercises one owned worktree operation.", "tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {"name": tool, "arguments": json.dumps(arguments)}}]} if tool else {"content": marker + "_DONE"}
                    for content, finish in ((delta, None), ({}, "tool_calls" if tool else "stop")):
                        chunk = {"id": f"worktree-{len(requests)}", "model": MODEL["id"], "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": content, "finish_reason": finish}]}
                        self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                    self.wfile.write(b"data: [DONE]\n\n")
                else:
                    def event(kind, value):
                        self.wfile.write(f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode())
                    event("message_start", {"type": "message_start", "message": {"id": f"msg_fixture_{len(requests)}", "type": "message", "role": "assistant", "model": "claude-opus-5", "content": [], "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 0}}})
                    block = {"type": "tool_use", "id": call_id, "name": tool, "input": {}} if tool else {"type": "text", "text": ""}
                    event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": block})
                    delta = {"type": "input_json_delta", "partial_json": json.dumps(arguments)} if tool else {"type": "text_delta", "text": marker + "_DONE"}
                    event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta})
                    event("content_block_stop", {"type": "content_block_stop", "index": 0})
                    event("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tool else "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 0}})
                    event("message_stop", {"type": "message_stop"})
                self.wfile.flush()
            except Exception as error:
                model_errors.append(str(error))
                self.json_response({"error": {"type": "fixture_refusal", "message": str(error)}}, 400)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    result = {"engine": args.engine, "status": "failed", "commercial_requests": 0, "transport": "localhost scripted provider", "staged_live_pilot": False}
    terminal = None
    try:
        with tempfile.TemporaryDirectory(prefix=f"{args.engine}-worktree-cards-") as temporary:
            root = Path(temporary).resolve()
            home, config, workspace, temp = [root / name for name in ("home", "config", "workspace", "temp")]
            for path in (home, config, workspace, temp):
                path.mkdir()
            environment = {key: value for key, value in os.environ.items() if not (
                key.endswith(("_API_KEY", "_AUTH_TOKEN", "_ACCESS_TOKEN")) or key.startswith(("HEYCODE_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")))}
            loopback = f"http://127.0.0.1:{server.server_port}"
            environment.update({"HOME": str(home), "TMPDIR": str(temp), "TERM": "xterm-256color", "COLORTERM": "truecolor", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null", "HTTP_PROXY": loopback, "HTTPS_PROXY": loopback, "NO_PROXY": "127.0.0.1,localhost"})
            environment.pop("NO_COLOR", None)
            def git(*command):
                return subprocess.run(["git", *command], cwd=workspace, env=environment, check=True, capture_output=True, text=True).stdout
            (workspace / "fixture.txt").write_text("WORKTREE_BASELINE\n")
            git("init", "-q")
            git("add", "fixture.txt")
            git("-c", "user.name=Local Fixture", "-c", "user.email=fixture@invalid", "commit", "-qm", "Fixture baseline")
            source_index = hashlib.sha256((workspace / ".git/index").read_bytes()).hexdigest()
            session_id = str(uuid.uuid4())
            if args.engine == "heycode":
                binary = args.binary.resolve()
                actual_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
                if actual_sha != args.sha256:
                    raise RuntimeError("immutable CLI digest differs")
                result["binary_sha256"] = actual_sha
                environment.update({"HEYCODE_HOME": str(config), "HEYCODE_WORKTREE_LOCAL_KEY": "fixture-only"})
                (config / "config.toml").write_text(f'''schema_version = 31
[llm]
provider = "openrouter"
model = "{MODEL['id']}"
api_key_env = "HEYCODE_WORKTREE_LOCAL_KEY"
base_url = "{loopback}/api/v1"
[approval]
mode = "full_access"
''')
                launch = [str(binary), "--no-background", "--trust-workspace"]
            else:
                binary = Path(shutil.which("claude") or "").resolve()
                if not binary.is_file():
                    raise RuntimeError("Claude executable unavailable")
                result["source_version"] = binary.name
                (config / ".claude.json").write_text(json.dumps({"hasCompletedOnboarding": True, "theme": "dark", "lastOnboardingVersion": "2.1.268"}))
                environment.update({"CLAUDE_CONFIG_DIR": str(config), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "CLAUDE_CODE_REMOTE_CONTROL": "0", "ANTHROPIC_BASE_URL": loopback, "ANTHROPIC_API_KEY": "local-only-dummy"})
                launch = [str(binary), "--restricted", "--strict-mcp-config", "--no-chrome", "--permission-mode", "manual", "--setting-sources", "project,local", "--settings", '{"remoteControlAtStartup":false}', "--tools", "EnterWorktree,ExitWorktree", "--session-id", session_id, "--name", "isolated-worktree-reference", "--model", "opus"]
            terminal = Terminal(launch, workspace, environment, output)
            terminal.stream = TerminalByteStream(terminal.screen)
            if args.engine == "heycode":
                terminal.wait("shift+tab to cycle")
            else:
                for attempt in range(8):
                    terminal.read(1.5)
                    visible = terminal.visible()
                    if "custom API key" in visible:
                        os.write(terminal.master, b"\x1b[A\r")
                    elif "trust this folder" in visible.lower():
                        os.write(terminal.master, b"\x1b[B\r")
                    elif any(text in visible.lower() for text in ("for shortcuts", "shift+tab", "try ")):
                        break
                    elif attempt == 7:
                        raise RuntimeError("Unrecognized isolated Claude startup")
            terminal.capture("00-ready")
            active = None
            for ordinal, marker in enumerate(MARKERS):
                os.write(terminal.master, (marker + ": Exercise this one worktree operation in the disposable local fixture.").encode())
                terminal.read(0.5)
                os.write(terminal.master, b"\r")
                deadline = time.monotonic() + 45
                while time.monotonic() < deadline:
                    terminal.read(0.2)
                    if model_errors:
                        raise RuntimeError(model_errors[-1])
                    if marker + "_DONE" in terminal.visible():
                        break
                    if terminal.process.poll() is not None:
                        raise RuntimeError("CLI exited during the worktree fixture")
                else:
                    raise RuntimeError("Worktree card did not settle: " + terminal.visible())
                terminal.capture(f"{ordinal+1:02d}-{marker.lower()}")
                inventory = git("worktree", "list", "--porcelain")
                paths = [Path(line.removeprefix("worktree ")) for line in inventory.splitlines() if line.startswith("worktree ")]
                if len(paths) != 2:
                    raise RuntimeError(f"expected source and exactly one retained worktree: {inventory}")
                created = next(path for path in paths if path != workspace)
                if active is not None and created != active:
                    raise RuntimeError("a refusal/exit unexpectedly replaced the owned worktree")
                active = created
                if (created / "fixture.txt").read_text() != "WORKTREE_BASELINE\n":
                    raise RuntimeError("worktree content differs from the owned fixture")
                if (workspace / "fixture.txt").read_text() != "WORKTREE_BASELINE\n":
                    raise RuntimeError("source fixture changed")
                (output / f"worktree-list-{ordinal+1}.txt").write_text(inventory)
                if args.inspect_details and args.engine == "heycode" and ordinal in (0, 2):
                    tool_name = expected_names[0 if ordinal == 0 else 1]
                    def header_row():
                        rows = [i for i, line in enumerate(terminal.screen.display)
                                if f"{tool_name}()" in line and "completed" in line]
                        return max(rows) if rows else None
                    row = header_row()
                    if row is None:
                        raise RuntimeError("completed worktree card has no visible mouse target")
                    compact_body = "".join("\n".join(terminal.screen.display[row+1:]).split())
                    complete_path = "".join(str(created).split()) in compact_body
                    explicit_elision = "…" in compact_body and created.name in compact_body
                    if not (complete_path or explicit_elision):
                        raise RuntimeError("compact worktree summary clips the destination/retained path without preserving its leaf")
                    def click_card(row):
                        column = 2
                        os.write(terminal.master, f"\x1b[<0;{column};{row+1}M\x1b[<0;{column};{row+1}m".encode())
                        terminal.read(0.4)
                    click_card(row)
                    frames = [terminal.capture(f"{ordinal+1:02d}-worktree-expanded")]
                    # Long receipts may exceed one viewport. Scroll the real
                    # transcript until the expanded card's start is visible.
                    for _ in range(12):
                        if header_row() is not None and '"cwd"' in terminal.visible():
                            break
                        os.write(terminal.master, b"\x1b[<64;20;15M" * 2)
                        terminal.read(0.25)
                        frames.append(terminal.visible())
                    frames.append(terminal.capture(f"{ordinal+1:02d}-worktree-expanded-start"))
                    status = "entered" if ordinal == 0 else "exited_retained"
                    needles = ('"cwd"', '"roots"', f'"status":"{status}"', str(created))
                    for _ in range(12):
                        combined = "".join("\n".join(frames).split())
                        if all("".join(needle.split()) in combined for needle in needles):
                            break
                        os.write(terminal.master, b"\x1b[<65;20;15M" * 2)
                        terminal.read(0.25)
                        frames.append(terminal.visible())
                    frames.append(terminal.capture(f"{ordinal+1:02d}-worktree-expanded-end"))
                    combined = "".join("\n".join(frames).split())
                    for needle in needles:
                        if "".join(needle.split()) not in combined:
                            raise RuntimeError(f"expanded original receipt is not inspectable: {needle}")
                    for _ in range(12):
                        if header_row() is not None:
                            break
                        os.write(terminal.master, b"\x1b[<64;20;15M" * 2)
                        terminal.read(0.25)
                    row = header_row()
                    if row is None:
                        raise RuntimeError("could not find expanded card header to collapse it")
                    click_card(row)
                    terminal.capture(f"{ordinal+1:02d}-worktree-collapsed-after-inspection")
            result.update({"status": "passed", "model_requests": len(requests), "tool_results": len(tool_results), "original_workspace": str(workspace), "retained_worktree": str(active), "source_index_unchanged": hashlib.sha256((workspace / ".git/index").read_bytes()).hexdigest() == source_index, "captures": terminal.captures})
            if not result["source_index_unchanged"]:
                raise RuntimeError("source index bytes changed")
            # Retain only fixture journals and workspace sidecars before teardown.
            journals = [p for p in config.rglob("*.jsonl") if p.is_file()]
            for ordinal, journal in enumerate(journals):
                shutil.copyfile(journal, output / f"session-{ordinal}.jsonl")
            sidecars = list(config.rglob("workspace.json"))
            for ordinal, sidecar in enumerate(sidecars):
                shutil.copyfile(sidecar, output / f"workspace-{ordinal}.json")
    except Exception as error:
        result.update({"status": "failed", "error": str(error)})
        if terminal:
            terminal.capture("failed")
    finally:
        if terminal:
            terminal.close()
        server.shutdown()
        server.server_close()
        result["local_requests"] = other_requests
        result["model_errors"] = model_errors
        (output / "tool-results.json").write_text(json.dumps(tool_results, indent=2))
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", choices=("heycode", "claude"), required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--sha256")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--inspect-details", action="store_true", help="Verify full native path summaries and actual mouse-expanded receipts")
    args = parser.parse_args()
    if args.engine == "heycode" and (args.binary is None or args.sha256 is None):
        parser.error("heycode requires an immutable --binary and --sha256")
    print(json.dumps(run(args), indent=2))
