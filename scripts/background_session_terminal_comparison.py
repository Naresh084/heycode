#!/usr/bin/env python3
"""Stepped, prompt-free Claude/heycode whole-session reference capture.

The controller accepts only reviewed slash commands and navigation keys. Claude
uses an explicit fresh session UUID in a disposable non-repository workspace.
Its existing authentication is inherited normally; credentials are never read,
copied, or printed. heycode uses an isolated home and a loopback catalog fixture.
Terminal PNGs render the actual captured PTY cells with one shared profile.
"""
from __future__ import annotations

import argparse
import contextlib
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
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

import pyte

from claude_terminal_reference import Screen
from session_background_pty import live as heycode_live
from task_console_pty import MODEL
from terminal_screenshot import render_screen


class Terminal:
    def __init__(self, command, workspace, environment, columns, rows):
        self.raw = bytearray()
        self.screen = Screen(columns, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        self.process = subprocess.Popen(command, cwd=workspace, env=environment,
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)

    def read(self, seconds=.5):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if not select.select([self.master], [], [], min(.05, max(0, until - time.monotonic())))[0]:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError:
                break
            if not data:
                break
            self.raw.extend(data)
            self.stream.feed(data)
        return "\n".join(self.screen.display)

    def send(self, data):
        os.write(self.master, data)

    def close(self):
        self.read(.1)
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGCONT)
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        os.close(self.master)


def run(binary, output, columns, rows, heycode_only=False):
    output.mkdir(parents=True, exist_ok=False)
    claude = shutil.which("claude")
    if not claude:
        raise RuntimeError("Claude CLI is unavailable")
    session_id = str(uuid.uuid4())
    actions, captures, claude_rows, cleanup, inventories = [], [], [], [], []
    provider_posts = []
    terminals = {}
    with contextlib.ExitStack():
        root = Path(tempfile.mkdtemp(prefix="heycode-bg-idle-compare-"))
        claude_workspace, heycode_workspace, home = (root / part for part in ("claude", "heycode", "home"))
        for directory in (claude_workspace, heycode_workspace, home):
            directory.mkdir()

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                data = json.dumps({"data": {"label": "local-only"}} if self.path.endswith("/key")
                    else ({"data": MODEL} if "/model/" in self.path else {"data": [MODEL], "total_count": 1, "links": {"next": None}})).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)

            def do_POST(self):
                provider_posts.append(self.path)
                self.send_error(403, "This idle reference must never request inference")

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_IDLE_FIXTURE"\nbase_url = "{base}"\n')
        common = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
        common.pop("NO_COLOR", None)
        heycode_environment = dict(common, HEYCODE_HOME=str(home), HEYCODE_IDLE_FIXTURE="local-only-fixture")
        for key in ("HEYCODE_SESSION_HOST", "HEYCODE_SESSION_HOST_TOKEN"):
            heycode_environment.pop(key, None)
        claude_environment = dict(common, CLAUDE_CODE_NO_FLICKER="1",
            CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
        claude_command = [claude, "--safe-mode", "--strict-mcp-config", "--no-chrome",
            "--setting-sources", "", "--settings", '{"remoteControlAtStartup":false}',
            "--model", "opus", "--permission-mode", "manual",
            "--tools", "", "--session-id", session_id, "--name", f"terminal-bg-idle-{session_id[:8]}"]
        heycode_command = [str(binary), "--trust-workspace", "--provider", "openrouter", "--model", MODEL["id"],
            "--approval", "default", "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_IDLE_FIXTURE"]

        def capture(target, label):
            terminal = terminals[target]
            visible = terminal.read(.6)
            stem = f"{len(captures) + 1:02d}-{target}-{label}"
            (output / f"{stem}.txt").write_text(visible)
            (output / f"{stem}.ansi").write_bytes(terminal.raw)
            render_screen(terminal.screen, output / f"{stem}.png")
            captures.append(stem)
            return {"capture": stem, "text": visible, "exit": terminal.process.poll()}

        def owned_claude():
            if heycode_only:
                return []
            result = subprocess.run([claude, "agents", "--cwd", str(claude_workspace), "--all", "--json"],
                cwd=claude_workspace, env=claude_environment, capture_output=True, text=True, timeout=10)
            if result.returncode:
                return {"error": "workspace-scoped Claude inventory failed", "returncode": result.returncode}
            parsed = json.loads(result.stdout)
            if not isinstance(parsed, list):
                raise RuntimeError("unexpected workspace-scoped Claude inventory schema")
            claude_rows[:] = parsed
            return parsed

        def snapshot():
            result = {"claude": owned_claude(), "heycode": heycode_live(home), "provider_posts": len(provider_posts)}
            inventories.append(result)
            return result

        def clear_composer(target):
            # The baseline heycode build maps Ctrl+U to undo. Use Backspace so
            # discovery also works before the root input-routing correction.
            # Explicit action=key,key=clear separately verifies that correction.
            terminals[target].send(b"\x7f" * 160 if target == "heycode" else b"\x15")
            terminals[target].read(.2)

        def safe_id(value):
            return str(uuid.UUID(value)) == value

        try:
            if not heycode_only:
                terminals["claude"] = Terminal(claude_command, claude_workspace, claude_environment, columns, rows)
            terminals["heycode"] = Terminal(heycode_command, heycode_workspace, heycode_environment, columns, rows)
            time.sleep(3)
            print(json.dumps({"session_id": None if heycode_only else session_id,
                "workspace": str(heycode_workspace if heycode_only else claude_workspace),
                "captures": [capture(target, "start") for target in terminals]}), flush=True)
            for line in iter(input, ""):
                request = json.loads(line)
                action = request["action"]
                if action == "finish":
                    break
                if action == "inventory":
                    print(json.dumps(snapshot()), flush=True)
                    continue
                target = request["target"]
                if target not in terminals:
                    raise ValueError("unknown owned terminal")
                terminal = terminals[target]
                if action == "discover":
                    command = request["command"]
                    if command not in ("background", "bg", "fork", "resume", "sessions", "tasks"):
                        raise ValueError("unreviewed discovery command")
                    clear_composer(target)
                    terminal.send(f"/{command}".encode())
                elif action == "execute":
                    command = request["command"]
                    allowed = {"background", "bg", "fork", "sessions", "tasks sessions", "quit", "exit"}
                    if command.startswith("resume "):
                        value = command.removeprefix("resume ")
                        ids = {session_id} if target == "claude" else {row["session_id"] for row in heycode_live(home)}
                        if not safe_id(value) or value not in ids:
                            raise ValueError("resume target is outside this capture's owned sessions")
                    elif command not in allowed:
                        raise ValueError("unreviewed slash command")
                    clear_composer(target)
                    terminal.send(f"/{command}\r".encode())
                elif action == "key":
                    data = {"escape": b"\x1b", "up": b"\x1b[A", "down": b"\x1b[B", "enter": b"\r",
                        "clear": b"\x15", "detach": b"\x1d", "ctrl_z": b"\x1a"}[request["key"]]
                    terminal.send(data)
                elif action != "capture":
                    raise ValueError("unknown action")
                actions.append(request)
                print(json.dumps(capture(target, request.get("label", action))), flush=True)
        except EOFError:
            pass
        finally:
            # Inspect only this fresh, nonce-bearing workspace. Never list or
            # signal an unrelated Claude session or read authentication files.
            scoped = owned_claude()
            for terminal in terminals.values():
                terminal.close()
            for host in heycode_live(home):
                result = subprocess.run([str(binary), "sessions", "stop", host["host_id"]],
                    cwd=heycode_workspace, env=heycode_environment, capture_output=True, text=True, timeout=15)
                cleanup.append({"app": "heycode", "id": host["host_id"], "code": result.returncode})
            # Retain a workspace while any Claude background ownership remains
            # unresolved; its process must be stopped before deleting its cwd.
            remaining = owned_claude()
            remaining_heycode = heycode_live(home)
            if not heycode_only and remaining == []:
                # Public CLI cleanup is restricted to this harness's unique
                # disposable project, including its own trust/history rows.
                purge_args = [claude, "project", "purge", str(claude_workspace.resolve())]
                plan = subprocess.run([*purge_args, "--dry-run"], env=claude_environment,
                    capture_output=True, text=True, timeout=10)
                (output / "claude-project-cleanup-dry-run.txt").write_text(plan.stdout + plan.stderr)
                if plan.returncode == 0:
                    purge = subprocess.run([*purge_args, "--yes"], env=claude_environment,
                        capture_output=True, text=True, timeout=10)
                    (output / "claude-project-cleanup.txt").write_text(purge.stdout + purge.stderr)
                    cleanup.append({"app": "claude_project", "workspace": str(claude_workspace.resolve()),
                        "code": purge.returncode})
                verify = subprocess.run([*purge_args, "--dry-run"], env=claude_environment,
                    capture_output=True, text=True, timeout=10)
                (output / "claude-project-cleanup-verified.txt").write_text(verify.stdout + verify.stderr)
            if remaining == [] and not remaining_heycode:
                shutil.rmtree(root)
            else:
                cleanup.append({"app": "claude", "workspace_retained_for_settlement": str(root)})
            server.shutdown(); server.server_close()
            with binary.open("rb") as executable:
                binary_hash = hashlib.file_digest(executable, "sha256").hexdigest()
            evidence = {"claude_version": subprocess.check_output([claude, "--version"], text=True).strip(),
                "claude_session_id": None if heycode_only else session_id,
                "claude_workspace": None if heycode_only else str(claude_workspace), "claude_owned_inventory": scoped,
                "binary": str(binary), "binary_sha256": binary_hash, "viewport": [columns, rows], "captures": captures,
                "actions": actions, "heycode_provider_posts": provider_posts, "cleanup": cleanup,
                "heycode_only": heycode_only, "inventories": inventories,
                "remaining_claude": remaining, "remaining_heycode": remaining_heycode,
                "claude_isolation": {"safe_mode": True, "tools": [], "remoteControlAtStartup": False,
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "setting_sources": []},
                "capture_method": "live PTY cells rendered with a shared terminal profile; not an OS screenshot"}
            (output / "evidence.json").write_text(json.dumps(evidence, indent=2))
            print(json.dumps({"finished": True, "evidence": str(output / "evidence.json"), "cleanup": cleanup}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=35)
    parser.add_argument("--heycode-only", action="store_true")
    args = parser.parse_args()
    run(args.binary.resolve(), args.out.resolve(), args.columns, args.rows, args.heycode_only)
