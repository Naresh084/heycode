#!/usr/bin/env python3
"""Real CLI/native-runtime task-console journey on a PTY and local HTTP provider.

Requires pyte. No provider account/network access; fresh disposable home/workspace.
Usage: python scripts/task_console_pty.py --binary target/debug/heycode --out /tmp/task-ui-pty
Evidence is raw PTY bytes plus reconstructed actual terminal screens, not mock frames.
"""
from __future__ import annotations
import argparse
import fcntl
import struct
import termios
import http.server
import json
import os
import re
from pathlib import Path
import tempfile
import threading
import time

import pyte
from tui_blackbox import FullScreenTui

MODEL = {
    "id": "z-ai/glm-5.3-flash", "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Fixture: Native Tasks", "created": 1787752741, "description": "Local deterministic test fixture",
    "context_length": 131072, "architecture": {"input_modalities": ["text"], "output_modalities": ["text"], "tokenizer": "Other", "instruct_type": None},
    "pricing": {"prompt": "0", "completion": "0"},
    "top_provider": {"context_length": 131072, "max_completion_tokens": 32768, "is_moderated": False},
    "supported_parameters": ["max_tokens", "temperature", "tool_choice", "tools", "reasoning"],
    "reasoning": {"mandatory": True, "default_enabled": True, "supported_efforts": ["max", "high", "low"], "default_effort": "max"},
}

class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, out: Path, *, color=False, columns=100, rows=45, theme="heycode-dark", work_only=False):
    out.mkdir(parents=True, exist_ok=True)
    requests = []
    gates = {name: threading.Event() for name in ("ALPHA", "BETA", "GAMMA")}
    started = {name: threading.Event() for name in gates}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            payload = {"data": {"label": "local test fixture"}} if self.path.endswith("/key") else ({"data": MODEL} if "/model/" in self.path else {"data": [MODEL], "total_count": 1, "links": {"next": None}})
            data = json.dumps(payload).encode()
            self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            (out / "requests.json").write_text(json.dumps(requests, indent=2))
            messages = request["messages"]
            human_indices = [i for i, message in enumerate(messages) if message["role"] == "user" and not str(message.get("content", "")).startswith("[job ")]
            users = [str(messages[i].get("content", "")) for i in human_indices]
            joined = "\n".join(users)
            last = users[-1] if users else ""
            tooltail = any(message["role"] == "tool" for message in messages[(human_indices[-1] + 1 if human_indices else 0):])
            self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()

            def chunk(delta, finish=None):
                if "tool_calls" in delta:
                    delta["reasoning"] = "Calling the requested native task tools."
                data = {"id": "task-ui-fixture", "object": "chat.completion.chunk", "model": MODEL["id"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(data) + "\n\n").encode()); self.wfile.flush()

            def tool(name, arguments, index=0, ident=None):
                return {"index": index, "id": ident or f"ui-{name}-{len(requests)}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}

            try:
                if "STEER_BETA" in joined:
                    chunk({"content": "STEER_ACK_BETA"}); chunk({}, "stop")
                elif any(f"TASK_UI_CHILD_{name}" in joined for name in gates):
                    name = next(name for name in gates if f"TASK_UI_CHILD_{name}" in joined)
                    if not tooltail:
                        chunk({"tool_calls": [tool("read", {"path":"activity-fixture.txt"})]}); chunk({}, "tool_calls")
                        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush(); return
                    chunk({"content": f"LIVE_TEXT_{name}", "reasoning": f"LIVE_REASONING_{name}"})
                    started[name].set()
                    gates[name].wait(90)
                    chunk({"content": f" FINISHED_{name}"}); chunk({}, "stop")
                elif "TASK_UI_START" in last and not tooltail:
                    calls = [tool("task", {"label": name.title(), "prompt": f"TASK_UI_CHILD_{name}", "provider": "native", "mode": "continuable"}, index, f"ui-task-{name.lower()}") for index, name in enumerate(gates)]
                    chunk({"tool_calls": calls}); chunk({}, "tool_calls")
                elif "TASK_UI_WORK" in last and not tooltail:
                    chunk({"tool_calls": [tool("task_create", {"request_key": "pty-review", "subject": "Review terminal result", "description": "Verify terminal output before accepting completion", "metadata": {"source": "pty"}})]}); chunk({}, "tool_calls")
                elif "TASK_UI_PROCESS" in last and not tooltail:
                    chunk({"tool_calls": [tool("background_terminal", {"label": "Echo terminal", "command": "printf 'PTY_READY\\n'; IFS= read -r line; printf 'PTY_INPUT:%s\\n' \"$line\""}), tool("background_terminal", {"label": "Other terminal", "command": "printf 'OTHER_READY\\n'; IFS= read -r line; printf 'OTHER_INPUT:%s\\n' \"$line\""}, index=1, ident="other-terminal")]}); chunk({}, "tool_calls")
                elif "TASK_UI_PROMOTE" in last and not tooltail:
                    chunk({"tool_calls": [tool("run_tool", {"tool": "bash", "arguments": {"command": "printf x >> invocation-count; printf 'PROMOTION_READY\\n'; while [ ! -f promotion.release ]; do sleep 0.05; done; printf 'PROMOTION_DONE\\n'"}})]}); chunk({}, "tool_calls")
                else:
                    chunk({"content": "PARENT_READY"}); chunk({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass  # Expected for a cancelled native provider stream.

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    os.environ["HEYCODE_TASK_UI_FIXTURE"] = "local-only-not-a-real-credential"
    evidence = []
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-task-home-") as home, tempfile.TemporaryDirectory(prefix="heycode-task-work-") as workspace:
            Path(workspace, "activity-fixture.txt").write_text("ACTUAL_CHILD_READ_RESULT\n")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            Path(home, "settings.toml").write_text(f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n')
            Path(home, "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_TASK_UI_FIXTURE"\nbase_url = "{base}"\n')
            tui = FullScreenTui(home, workspace, str(binary), fake=False, rows=rows, columns=columns, color=color, extra=["--provider", "openrouter", "--model", MODEL["id"], "--approval", "full_access", "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_TASK_UI_FIXTURE"])
            screen = Screen(columns, rows); stream = pyte.ByteStream(screen)

            def read(seconds=.1):
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def send(data):
                os.write(tui.fd, data)

            def resize(columns, rows):
                screen.resize(rows, columns)
                fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
                read(.3)

            def wait(needle, timeout=25):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    text = read()
                    if needle in text:
                        return text
                    if not tui.alive():
                        raise AssertionError(f"CLI exited while waiting for {needle}:\n{text}")
                raise AssertionError(f"Missing {needle}:\n{text}")

            def capture(name):
                text = read(.15)
                (out / f"{name}.txt").write_text(text)
                (out / f"{name}.ansi").write_bytes(tui.transcript)
                from terminal_screenshot import render_screen
                render_screen(screen, out / f"{name}.png", background="#ffffff" if theme == "heycode-light" else "#101014", foreground="#24292f" if theme == "heycode-light" else "#dddddd")
                evidence.append(name)
                return text

            def open_category(label):
                read(.2)
                patterns = {"Jobs": r"\d+ shells?", "Agents": r"(?:← )?\d+ agents?", "Work": r"\d+ tasks?", "Teams": r"\d+ teams?"}
                for row, line in reversed(list(enumerate(screen.display))):
                    match = re.search(patterns[label], line)
                    if match:
                        send(f"\x1b[<0;{match.start()+1};{row+1}M".encode()); read(.2); return
                if label == "Agents":
                    send(b"\x14"); read(.2); return
                raise AssertionError(f"No category {label}:\n" + "\n".join(screen.display))

            def open_row(label, foreground=True):
                for _ in range(80):
                    read(.06)
                    for row, line in enumerate(screen.display):
                        if label in line and ("@" in line or "(" in line or "⏺" in line or "◯" in line):
                            send(f"\x1b[<0;{line.index(label)+1};{row+1}M".encode()); read(.12)
                            if foreground and "f to foreground" in "\n".join(screen.display):
                                if label == "Alpha" and "agent-inspector" not in evidence:
                                    inspector = capture("agent-inspector")
                                    assert ("Recent activity" in inspector or "Progress" in inspector) and "read" in inspector.lower(), inspector
                                    resize(45, 30); capture("agent-inspector-narrow"); resize(columns, rows)
                                send(b"f")
                            return
                    send(b"\x1b[B")
                raise AssertionError(f"No selectable task {label}:\n" + "\n".join(screen.display))

            try:
                initial = read(2)
                if "Welcome to heycode" in initial:
                    send(b"\x1b[B\x1b[B\r"); wait("Select a provider")
                    send(b"OpenRouter\r"); wait("Paste your OpenRouter API key")
                    send(b"local-only-not-a-real-credential\r"); wait("Choose a model"); send(b"\r")
                wait("Full access")
                if work_only:
                    send(b"TASK_UI_WORK\r"); wait("PARENT_READY"); wait("1 task")
                    open_category("Work"); open_row("Review terminal result")
                    detail = wait("Verify terminal output before accepting completion")
                    capture("work-detail")
                    assert "Work · Review terminal result (pending)" in detail, detail
                    assert re.search(r"Work ID: work-[0-9a-f]{64}", detail), detail
                    assert "Revision" in detail and "Owner" in detail, detail
                    assert "Interrupt]" not in detail and "terminal input" not in detail.lower(), detail
                    assert "foreground" not in detail.lower() and "elapsed" not in detail.lower(), detail
                    result = {"passed": True, "screens": evidence, "provider_requests": len(requests), "binary": str(binary), "runtime": "native", "transport": "localhost HTTP fixture", "scope": "work-only"}
                    (out / "result.json").write_text(json.dumps(result, indent=2))
                    print(json.dumps(result, indent=2))
                    return
                send(b"TASK_UI_START\r"); wait("PARENT_READY")
                assert all(event.wait(4) for event in started.values())
                wait("PARENT_READY"); capture("spawn-trees")
                send(b"PARENT_DRAFT_UNSENT"); open_category("Agents"); listing = capture("agents-list")
                assert "Background" in listing and "PARENT_DRAFT_UNSENT" not in listing
                open_row("Alpha"); wait("LIVE_TEXT_ALPHA"); capture("alpha-live")
                send(b"CHILD_ALPHA_DRAFT"); read(.2)
                open_category("Agents"); open_row("Beta"); wait("LIVE_TEXT_BETA"); capture("beta-live")
                send(b"STEER_BETA_ONE\r"); wait("Press up to edit queued messages")
                send(b"STEER_BETA_TWO\r"); wait("STEER_BETA_TWO"); queued = capture("beta-two-queued")
                assert queued.index("heycode 0.1.0") < queued.index("STEER_BETA_ONE") < queued.index("◯ main")
                send(b"\x1b[A"); read(.3)
                recalled = capture("beta-recalled")
                assert "STEER_BETA_ONE" in recalled and "STEER_BETA_TWO" in recalled
                assert "Press up to edit queued messages" not in recalled
                send(b"\x03"); wait("idle"); capture("beta-cancelled-draft-preserved")
                assert not gates["BETA"].is_set()
                send(b"\x03"); read(.2); send(b"STEER_BETA\r"); wait("STEER_ACK_BETA"); capture("beta-follow-up")
                send(b"\x1b[1;3D"); wait("PARENT_DRAFT_UNSENT")
                open_category("Agents"); open_row("Alpha"); wait("CHILD_ALPHA_DRAFT"); capture("alpha-draft-preserved")
                resize(45, 30); capture("alpha-narrow"); resize(columns, rows)
                send(b"\x1b[1;3D"); wait("PARENT_DRAFT_UNSENT"); capture("parent-draft-preserved")
                send(b"\x1b"); read(.2); send(b"\x03"); read(.2); send(b"TASK_UI_PROCESS\r"); wait("PARENT_READY"); read(.4); open_category("Jobs"); shell_list = capture("shells-list"); assert "PTY_READY" not in shell_list; open_row("Echo terminal", foreground=False)
                detail = wait("PTY_READY"); capture("terminal-live"); assert "OTHER_READY" not in detail
                send(b"\x1b[D"); returned = capture("shells-list-return"); assert "Echo terminal" in returned and "PTY_READY" not in returned
                open_row("Other terminal", foreground=False); other = wait("OTHER_READY"); capture("other-terminal-detail"); assert "PTY_READY" not in other
                send(b"\x1b[D"); read(.2); open_row("Echo terminal", foreground=False); send(b"f"); wait("PTY_READY")
                send(b"hello terminal\r"); wait("PTY_INPUT:hello terminal"); wait("completed"); capture("terminal-input-completed")
                send(b"\x1b[1;3D"); read(.2); send(b"\x1b"); read(.2); send(b"\x03"); read(.2)
                send(b"TASK_UI_WORK\r"); wait("1 task"); open_category("Work"); open_row("Review terminal result")
                detail = wait("Verify terminal output before accepting completion"); capture("work-detail")
                assert "Interrupt]" not in detail and "terminal input" not in detail.lower()
                for gate in gates.values(): gate.set()
                result = {"passed": True, "screens": evidence, "provider_requests": len(requests), "binary": str(binary), "runtime": "native", "transport": "localhost HTTP fixture"}
                (out / "result.json").write_text(json.dumps(result, indent=2))
                print(json.dumps(result, indent=2))
            except Exception:
                capture("failure")
                raise
            finally:
                for gate in gates.values(): gate.set()
                tui.close()
    finally:
        server.shutdown(); server.server_close()

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--theme", default="heycode-dark", choices=["heycode-dark", "heycode-light"])
    parser.add_argument("--color", action="store_true")
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=45)
    parser.add_argument("--work-only", action="store_true")
    args = parser.parse_args()
    run(args.binary.resolve(), args.out.resolve(), color=args.color, columns=args.columns, rows=args.rows, theme=args.theme, work_only=args.work_only)
