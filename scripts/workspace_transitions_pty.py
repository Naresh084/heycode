#!/usr/bin/env python3
"""Disposable actual-PTY evidence for native workspace commands and Claude add-dir.

heycode uses an immutable CLI and a deterministic localhost SSE provider. Claude
receives only local slash commands, an isolated home/config/workspace, disabled
RC, and a localhost API endpoint. No commercial inference is requested.
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
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

import pyte
from memory_skills_pty import MODEL
from terminal_screenshot import render_screen

PINNED_SHA = "ec95c4bbc96501ca627e470f0126915387fee9513cdf14f57cd0b419cea8e903"


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class Terminal:
    def __init__(self, args: list[str], cwd: Path, environment: dict[str, str], output: Path):
        self.output = output
        self.output.mkdir(parents=True, exist_ok=True)
        self.transcript = bytearray()
        self.captures: list[str] = []
        self.screen = Screen(126, 46)
        self.stream = pyte.ByteStream(self.screen)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 46, 126, 0, 0))
        self.process = subprocess.Popen(args, cwd=cwd, env=environment, stdin=slave,
                                        stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)

    def read(self, seconds: float = 0.2):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], min(0.1, max(0, deadline-time.monotonic())))
            if ready:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if not data:
                    break
                self.transcript.extend(data)
                self.stream.feed(data)

    def visible(self):
        return "\n".join(self.screen.display)

    def wait(self, needle: str, timeout: float = 35):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read()
            if "".join(needle.split()) in "".join(self.visible().split()):
                return self.visible()
            if self.process.poll() is not None:
                raise AssertionError(f"CLI exited while waiting for {needle!r}: {self.visible()}")
        raise AssertionError(f"Timed out waiting for {needle!r}: {self.visible()}")

    def send(self, command: str):
        os.write(self.master, command.encode() + b"\r")

    def capture(self, name: str):
        self.read(0.4)
        (self.output / f"{name}.txt").write_text(self.visible())
        render_screen(self.screen, self.output / f"{name}.png")
        self.captures.append(name)
        return self.visible()

    def close(self):
        (self.output / "terminal.ansi").write_bytes(self.transcript)
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)


def environment(home: Path):
    value = os.environ.copy()
    value.update({"HOME": str(home), "TERM": "xterm-256color", "COLORTERM": "truecolor"})
    value.pop("NO_COLOR", None)
    return value


def run_heycode(binary: Path, output: Path):
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == PINNED_SHA, "unexpected CLI artifact"
    requests: list[dict] = []
    paths: dict[str, str] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path.endswith("/key"):
                payload = {"data": {"label": "workspace-pty-local"}}
            elif "/model/" in self.path:
                payload = {"data": MODEL}
            else:
                payload = {"data": [MODEL], "total_count": 1, "links": {"next": None}}
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            messages = request.get("messages", [])
            last_user = next((m.get("content", "") for m in reversed(messages) if m.get("role") == "user"), "")
            marker = next((m for m in ("PROBE_DENIED", "PROBE_ALLOWED", "PROBE_GUIDANCE_BEFORE", "PROBE_GUIDANCE_AFTER", "PROBE_RECOMPOSE", "PROBE_OUTSIDE") if m in str(last_user)), "AUXILIARY")
            after_tool = bool(messages and messages[-1].get("role") == "tool")
            if marker in ("PROBE_DENIED", "PROBE_ALLOWED") and not after_tool:
                delta = {"reasoning": "Deterministic local fixture exercises the requested file boundary.", "tool_calls": [{"index": 0, "id": f"call-{marker}", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": paths["outside_file"]})}}]}
                reason = "tool_calls"
            else:
                text = marker + "_DONE"
                if after_tool:
                    raw = str(messages[-1].get("content", ""))
                    success = "AUTHORIZED_OUTSIDE_FILE" in raw
                    text += " AUTHORIZED_OUTSIDE_FILE" if success else " READ_REFUSED"
                delta = {"content": text}
                reason = "stop"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for content, finish in [(delta, None), ({}, reason)]:
                chunk = {"id": f"workspace-{len(requests)}", "model": MODEL["id"], "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": content, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    output.mkdir(parents=True, exist_ok=True)
    result: dict = {"engine": "heycode", "binary_sha256": PINNED_SHA, "commercial_requests": 0}
    terminal = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-workspace-pty-") as folder:
            root = Path(folder).resolve()
            home, workspace, outside = root / "home", root / "work", root / "outside"
            nested = workspace / "nested dir"
            for path in (home, nested, outside):
                path.mkdir(parents=True)
            (workspace / "AGENTS.md").write_text("ROOT_GUIDANCE_RETIRED")
            (nested / "AGENTS.md").write_text("NESTED_GUIDANCE_BEFORE")
            (outside / "AGENTS.md").write_text("OUTSIDE_MUST_NOT_BE_TRUSTED")
            (outside / "allowed.txt").write_text("AUTHORIZED_OUTSIDE_FILE")
            paths["outside_file"] = str(outside / "allowed.txt")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(f'''schema_version = 31
[llm]
provider = "openrouter"
model = "{MODEL['id']}"
api_key_env = "HEYCODE_WORKSPACE_PTY_KEY"
base_url = "{base}"
[approval]
mode = "full_access"
''')
            env = environment(home)
            env.update({"HEYCODE_HOME": str(home), "HEYCODE_WORKSPACE_PTY_KEY": "local-only-not-real"})
            terminal = Terminal([str(binary.resolve()), "--no-background", "--trust-workspace", "--provider", "openrouter", "--model", MODEL["id"], "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_WORKSPACE_PTY_KEY"], workspace, env, output)
            terminal.wait("shift+tab to cycle")
            terminal.send("/rename Workspace boundary fixture")
            terminal.wait("Renamed to Workspace boundary fixture")
            terminal.capture("00-start")
            os.write(terminal.master, b"/add-dir")
            terminal.read(0.7)
            terminal.capture("00a-add-directory-command-menu")
            os.write(terminal.master, b"\r")
            terminal.read(0.7)
            no_argument = terminal.capture("00b-add-directory-no-argument")
            assert "required" in no_argument.lower(), no_argument

            def session():
                files = list((home / "sessions").rglob("session.jsonl"))
                assert len(files) == 1, files
                return files[0]

            original_session = session()

            def state():
                path = original_session.parent / "workspace.json"
                return json.loads(path.read_text()) if path.exists() else None

            def expect_cwd(path: Path):
                deadline = time.monotonic() + 25
                while time.monotonic() < deadline:
                    terminal.read()
                    current = state()
                    if current and current["snapshot"]["cwd"] == str(path):
                        return current
                raise AssertionError(f"cwd did not become {path}: {terminal.visible()}")

            def probe(marker: str):
                start = len(requests)
                terminal.send(marker)
                terminal.wait(marker + "_DONE")
                selected = requests[start:]
                assert selected, marker
                return selected

            probe("PROBE_DENIED")
            terminal.wait("READ_REFUSED")
            terminal.capture("01-read-before-grant-refused")
            terminal.send(f"/cd {outside}")
            terminal.wait("outside the authorized")
            terminal.capture("02-cd-before-grant-refused")
            assert state() is None
            terminal.send(f"/add-dir {root / 'does-not-exist'}")
            terminal.wait("Directory is unavailable")
            terminal.capture("03-add-missing-refused")
            assert state() is None
            terminal.send(f"/add-dir {outside}")
            terminal.wait("revision 1")
            terminal.capture("04-add-directory-granted")
            granted = state()
            assert granted["snapshot"]["cwd"] == str(workspace)
            assert str(outside) in [r["path"] for r in granted["snapshot"]["roots"]]
            probe("PROBE_ALLOWED")
            terminal.wait("PROBE_ALLOWED_DONE AUTHORIZED_OUTSIDE_FILE")
            terminal.capture("05-authorized-read-result")
            terminal.send('/cd "nested dir"')
            expect_cwd(nested)
            terminal.capture("06-current-cwd-nested")
            before = probe("PROBE_GUIDANCE_BEFORE")[0]
            system = "\n".join(str(m.get("content", "")) for m in before["messages"] if m["role"] == "system")
            assert "NESTED_GUIDANCE_BEFORE" in system and "ROOT_GUIDANCE_RETIRED" not in system
            (nested / "AGENTS.md").write_text("NESTED_GUIDANCE_AFTER")
            terminal.send("/memory show project:agents")
            terminal.wait("NESTED_GUIDANCE_AFTER")
            terminal.capture("06b-current-instruction-source")
            after = probe("PROBE_GUIDANCE_AFTER")[0]
            system = "\n".join(str(m.get("content", "")) for m in after["messages"] if m["role"] == "system")
            assert "NESTED_GUIDANCE_AFTER" in system and "NESTED_GUIDANCE_BEFORE" not in system
            terminal.capture("07-guidance-refreshed")
            before_restart = original_session.read_bytes()
            offset = len(terminal.transcript)
            terminal.send("/reload-plugins")
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                terminal.read()
                raw = terminal.transcript[offset:]
                if b"\x1b[?1049l" in raw and b"\x1b[?1049h" in raw and "shift+tab to cycle" in terminal.visible():
                    break
                if terminal.process.poll() is not None:
                    raise AssertionError(f"recomposition exited: {terminal.visible()}")
            else:
                raise AssertionError(f"recomposition did not reenter terminal: {terminal.visible()}")
            assert session() == original_session and original_session.read_bytes().startswith(before_restart)
            expect_cwd(nested)
            terminal.send("/worktree status")
            terminal.wait("revision 2")
            terminal.capture("08-reloaded-same-session-cwd")
            reloaded = probe("PROBE_RECOMPOSE")[0]
            assert "NESTED_GUIDANCE_AFTER" in json.dumps(reloaded)
            terminal.send(f"/cd {outside}")
            expect_cwd(outside)
            external = probe("PROBE_OUTSIDE")[0]
            system = "\n".join(str(m.get("content", "")) for m in external["messages"] if m["role"] == "system")
            assert "OUTSIDE_MUST_NOT_BE_TRUSTED" not in system and "NESTED_GUIDANCE_AFTER" not in system
            terminal.capture("09-outside-cwd-without-project-trust")
            assert session() == original_session
            (output / "workspace-final.json").write_text(json.dumps(state(), indent=2))
            (output / "session.jsonl").write_bytes(original_session.read_bytes())
            result.update({"status": "passed", "session_id": original_session.parent.name, "localhost_provider_requests": len(requests), "same_session_reload_after_cd": True, "read_before_grant_refused": True, "read_after_grant_succeeded": True, "guidance_refreshed": True, "outside_project_trust_denied": True})
    except Exception as error:
        result.update({"status": "failed", "error": f"{type(error).__name__}: {error}"})
        if terminal:
            terminal.capture("failed")
    finally:
        if terminal:
            result["captures"] = terminal.captures
            terminal.close()
        server.shutdown()
        server.server_close()
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


def run_claude(output: Path):
    output.mkdir(parents=True, exist_ok=True)
    executable = shutil.which("claude")
    assert executable, "Claude unavailable"
    terminal = None
    api_requests: list[dict] = []

    class Reject(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            api_requests.append({"path": self.path, "body": body.decode("utf-8", "replace")})
            self.send_error(503, "Prompt-free reference has no inference provider")
        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Reject)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    result = {"engine": "claude", "model_prompt_submitted": False, "commercial_requests": 0, "remote_control_at_startup": False}
    try:
        with tempfile.TemporaryDirectory(prefix="claude-workspace-reference-") as folder:
            root = Path(folder).resolve()
            home, config, work, extra = [root / name for name in ("home", "config", "work", "extra")]
            for path in (home, config, work, extra):
                path.mkdir()
            (config / ".claude.json").write_text(json.dumps({"hasCompletedOnboarding": True, "theme": "dark", "lastOnboardingVersion": "2.1.268"}))
            env = environment(home)
            for key in list(env):
                if key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")):
                    env.pop(key)
            env.update({"CLAUDE_CONFIG_DIR": str(config), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}", "ANTHROPIC_API_KEY": "local-prompt-free-fixture"})
            session_id = str(uuid.uuid4())
            args = [executable, "--permission-mode", "manual", "--strict-mcp-config", "--no-chrome", "--setting-sources", "project,local", "--settings", '{"remoteControlAtStartup":false}', "--session-id", session_id, "--name", "isolated-add-directory-reference", "--model", "opus"]
            terminal = Terminal(args, work, env, output)
            terminal.read(5)
            startup = terminal.capture("00-startup")
            if "custom API key" in startup:
                # Explicitly accept only this script's dummy localhost key.
                os.write(terminal.master, b"\x1b[A\r")
                terminal.read(3)
                startup = terminal.capture("00b-local-fixture-key-accepted")
            if "trust" in startup.lower() and "folder" in startup.lower():
                os.write(terminal.master, b"\x1b[B\r")
                terminal.read(3)
            if "custom API key" in terminal.visible():
                os.write(terminal.master, b"\x1b[A\r")
                terminal.read(3)
            idle = terminal.capture("01-idle")
            if any(marker in idle.lower() for marker in ("select a theme", "welcome to claude code", "sign in")):
                raise RuntimeError("isolated startup/auth boundary prevents prompt-free reference")
            if not any(marker in idle.lower() for marker in ("for shortcuts", "shift+tab", "try ")):
                raise RuntimeError("isolated Claude did not reach an identifiable idle composer")
            os.write(terminal.master, b"/add-dir")
            terminal.read(0.6)
            terminal.capture("02-add-directory-command-menu")
            os.write(terminal.master, b"\r")
            terminal.read(1)
            opened = terminal.capture("03-add-directory-open")
            if "directory" not in opened.lower():
                raise AssertionError("no recognizable add-directory local surface")
            os.write(terminal.master, b"\x1b")
            terminal.read(0.4)
            os.write(terminal.master, f"/add-dir {extra}".encode())
            terminal.read(0.7)
            os.write(terminal.master, b"\r")
            terminal.read(1.5)
            success = terminal.capture("04-add-directory-result")
            if "Yes, for this session" in success:
                # The selected session-only grant applies to the disposable
                # directory; never choose the persistent-directory option.
                os.write(terminal.master, b"\r")
                terminal.read(1.5)
                success = terminal.capture("04b-add-directory-confirmed")
            if not any(marker in success.lower() for marker in ("added", "now accessible", "added working directory")):
                raise AssertionError("add-directory result was not recognizable")
            os.write(terminal.master, f"/add-dir {root / 'does-not-exist'}".encode())
            terminal.read(0.7)
            os.write(terminal.master, b"\r")
            terminal.read(1)
            missing = terminal.capture("05-add-missing-result")
            normalized_missing = " ".join(missing.lower().split())
            if not any(marker in normalized_missing for marker in ("not found", "does not exist", "not exist", "invalid directory")):
                raise AssertionError("missing-directory refusal was not recognizable")
            assert not api_requests, "reference unexpectedly attempted inference"
            result.update({"status": "captured", "session_id": session_id, "version": subprocess.run([executable, "--version"], capture_output=True, text=True, check=True).stdout.strip(), "local_api_requests": len(api_requests)})
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
        if terminal:
            terminal.capture("blocked")
    finally:
        if terminal:
            result["captures"] = terminal.captures
            terminal.close()
        server.shutdown()
        server.server_close()
        (output / "local-api-requests.json").write_text(json.dumps(api_requests, indent=2))
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=("heycode", "claude"), required=True)
    parser.add_argument("--binary", type=Path, default=Path("tmp/cli-snapshots/ec95c4bbc96501ca/dshx"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    answer = run_heycode(args.binary, args.output) if args.engine == "heycode" else run_claude(args.output)
    print(json.dumps(answer, indent=2))
    raise SystemExit(0 if answer["status"] in ("passed", "captured") else 1)
