#!/usr/bin/env python3
"""Stepped Claude command reference over explicitly seeded disposable history.

No conversation prompt is accepted. A dummy credential and refusing loopback
endpoint guard against a command accidentally requesting inference. Captures
prove the actual source UI, not provider execution or real model-generated history.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import threading
import time
import uuid

from claude_compaction_terminal_reference import ClaudePty


def run(output: Path):
    output.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which("claude")).resolve()
    requests, actions, captures = [], [], []
    result = {"source_binary": str(executable), "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
              "history": "two explicitly seeded local user/assistant pairs", "commercial_calls": 0}
    terminal = None

    class RefuseRequests(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            requests.append({"method": "POST", "path": self.path})
            self.send_error(403, "Inference is forbidden in this command reference")
        def do_GET(self):
            requests.append({"method": "GET", "path": self.path})
            self.send_error(403, "External services are outside this command reference")
        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), RefuseRequests)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    with tempfile.TemporaryDirectory(prefix="claude-lifecycle-reference-") as folder:
        root = Path(folder)
        config, workspace = root / "config", root / "workspace"
        config.mkdir(); workspace.mkdir()
        (config / ".claude.json").write_text(json.dumps({"hasCompletedOnboarding": True, "theme": "dark", "lastOnboardingVersion": executable.name}))
        env = os.environ.copy()
        for key in list(env):
            if key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")) or key.endswith(("_API_KEY", "_AUTH_TOKEN")):
                env.pop(key)
        env.update(CLAUDE_CONFIG_DIR=str(config), TERM="xterm-256color", COLORTERM="truecolor",
                   CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1", CLAUDE_CODE_REMOTE_CONTROL="0", CLAUDE_CODE_NO_FLICKER="1",
                   ANTHROPIC_BASE_URL=f"http://127.0.0.1:{server.server_port}", ANTHROPIC_API_KEY="local-only-dummy",
                   HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        env.pop("NO_COLOR", None)
        common = ["--safe-mode", "--strict-mcp-config", "--no-chrome", "--setting-sources", "project,local",
                  "--settings", '{"remoteControlAtStartup":false}', "--permission-mode", "manual", "--model", "opus"]
        session_id = str(uuid.uuid4())
        try:
            terminal = ClaudePty(str(executable), env, workspace, [*common, "--session-id", session_id, "--name", "Lifecycle source original"], columns=100, rows=36)
            terminal.ready()
            terminal.send(b"/copy\r")
            terminal.wait_for("No assistant message to copy")
            terminal.send(b"/exit\r")
            deadline = time.monotonic() + 5
            while terminal.process.poll() is None and time.monotonic() < deadline:
                terminal.read()
            terminal.close()
            (output / "bootstrap.ansi").write_bytes(terminal.transcript)
            terminal = None
            logs = list(config.glob(f"projects/**/{session_id}.jsonl"))
            assert len(logs) == 1, logs
            log = logs[0]
            existing = [json.loads(line) for line in log.read_text().splitlines() if line]
            parent = next((row["uuid"] for row in reversed(existing) if "uuid" in row), None)
            with log.open("a") as stream:
                for index, prompt in enumerate(("Lifecycle first checkpoint", "Lifecycle second checkpoint")):
                    user_id, assistant_id = str(uuid.uuid4()), str(uuid.uuid4())
                    shared = {"isSidechain": False, "userType": "external", "cwd": str(workspace.resolve()), "sessionId": session_id,
                              "version": executable.name, "gitBranch": "", "timestamp": f"2026-09-12T00:00:0{index}.000Z", "fixtureSeeded": True}
                    user = {**shared, "parentUuid": parent, "type": "user", "message": {"role": "user", "content": prompt}, "uuid": user_id}
                    assistant = {**shared, "parentUuid": user_id, "type": "assistant", "uuid": assistant_id, "requestId": f"local_fixture_{index}",
                                 "message": {"model": "local-fixture-no-inference", "id": f"msg_local_fixture_{index}", "type": "message", "role": "assistant",
                                             "content": [{"type": "text", "text": "Synthetic local response; no model was called."}], "stop_reason": "end_turn", "stop_sequence": None,
                                             "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}}
                    for row in (user, assistant):
                        stream.write(json.dumps(row) + "\n")
                    parent = assistant_id
            (output / "seeded-session.jsonl").write_bytes(log.read_bytes())
            terminal = ClaudePty(str(executable), env, workspace, [*common, "--resume", session_id], columns=100, rows=36)
            terminal.ready()
            terminal.capture(output, "00-resumed-seeded-history")
            captures.append("00-resumed-seeded-history")
            print(json.dumps({"ready": True, "session_id": session_id, "text": terminal.visible()}), flush=True)
            keys = {"enter": b"\r", "up": b"\x1b[A", "down": b"\x1b[B", "escape": b"\x1b", "page_up": b"\x1b[5~", "page_down": b"\x1b[6~"}
            allowed = {"/resume", "/branch Lifecycle source branch", "/resume Lifecycle source original", "/resume Lifecycle source", f"/resume {session_id}", "/rewind", "/clear"}
            for raw in sys.stdin:
                action = json.loads(raw)
                if action.get("stop"):
                    result["status"] = "captured"
                    break
                if "command" in action:
                    assert action["command"] in allowed, "Unreviewed command refused"
                    terminal.send(b"\x15")  # Clear only this fixture's unsent rewind draft.
                    terminal.send(action["command"].encode() + b"\r")
                elif "key" in action:
                    terminal.send(keys[action["key"]])
                elif "click_text" in action:
                    matches = [(i, line) for i, line in enumerate(terminal.screen.display) if action["click_text"] in line and (not action.get("exclude") or action["exclude"] not in line)]
                    assert len(matches) == 1, matches
                    row, line = matches[0]
                    column = line.index(action["click_text"]) + len(action["click_text"]) - len(action["click_text"].lstrip())
                    terminal.send(f"\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m".encode())
                else:
                    assert action.get("read") is True
                terminal.read(.7)
                assert not requests, requests
                name = action.get("capture", f"state-{len(captures):02d}")
                assert name.replace("-", "").replace("_", "").isalnum(), "Invalid capture name"
                terminal.capture(output, name)
                actions.append(action); captures.append(name)
                print(json.dumps({"capture": name, "text": terminal.visible(), "exit": terminal.process.poll()}), flush=True)
        except Exception as error:
            result.update(status="failed", failure=f"{type(error).__name__}: {error}")
            if terminal is not None:
                terminal.capture(output, "failed")
        finally:
            if terminal is not None:
                terminal.close()
                (output / "terminal.ansi").write_bytes(terminal.transcript)
                result["owned_terminal_alive"] = terminal.process.poll() is None
            server.shutdown(); server.server_close(); server_thread.join(timeout=3)
            journals = {str(path.relative_to(config)): path.read_text() for path in config.glob("projects/**/*.jsonl")}
            (output / "journals.json").write_text(json.dumps(journals, indent=2))
            result.update(actions=actions, captures=captures, loopback_requests=requests, source_session_id=session_id)
            result.setdefault("status", "stopped")
            (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result), flush=True)
    if result["status"] == "failed":
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    run(parser.parse_args().output)
