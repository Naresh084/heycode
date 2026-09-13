#!/usr/bin/env python3
"""Capture Claude hook, init, and tool-inspection controls without model I/O.

Every child uses disposable home/config/workspace roots, a dummy key, and a
loopback-only Anthropic endpoint. The harness never submits a conversation
prompt, never executes `/init` or `/context`, and asserts that the source CLI
made no Messages inference request and wrote no project artifact. Local-only
token-count requests are recorded separately because opening native controls
may use them for context accounting without scheduling inference.
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
import socketserver
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid

import pyte

from terminal_screenshot import TerminalByteStream, render_screen


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class LoopbackServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True

    def __init__(self, address):
        super().__init__(address, LoopbackHandler)
        self.requests: list[dict[str, object]] = []


class LoopbackHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        self.server.requests.append(  # type: ignore[attr-defined]
            {"path": self.path, "body_bytes": len(body)}
        )
        payload = b'{"type":"error","error":{"type":"fixture","message":"local only"}}'
        self.send_response(503)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, _format, *_args):
        return


def sanitized_environment(root: Path, port: int) -> dict[str, str]:
    environment = {
        name: value
        for name, value in os.environ.items()
        if not any(
            marker in name.upper()
            for marker in ("API_KEY", "ACCESS_TOKEN", "AUTH_TOKEN", "OAUTH_TOKEN")
        )
    }
    environment.update(
        {
            "HOME": str(root / "home"),
            "CLAUDE_CONFIG_DIR": str(root / "config"),
            "ANTHROPIC_API_KEY": "sk-ant-heycode-hooks-init-reference-not-real",
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "CLAUDE_CODE_REMOTE_CONTROL": "0",
            "CLAUDE_CODE_NO_FLICKER": "1",
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
        }
    )
    environment.pop("NO_COLOR", None)
    return environment


def run_child(
    executable: Path,
    output: Path,
    root: Path,
    environment: dict[str, str],
    actions,
    *,
    columns: int,
    rows: int,
) -> dict[str, object]:
    output.mkdir(parents=True, exist_ok=False)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
    process = subprocess.Popen(
        [
            str(executable),
            "--restricted",
            "--strict-mcp-config",
            "--no-chrome",
            "--setting-sources",
            "project,local",
            "--permission-mode",
            "manual",
            "--session-id",
            str(uuid.uuid4()),
            "--name",
            "hooks-init-reference",
        ],
        cwd=root / "workspace",
        env=environment,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        start_new_session=True,
        close_fds=True,
    )
    os.close(slave)
    raw = bytearray()
    captures: list[str] = []
    screen = Screen(columns, rows)
    stream = TerminalByteStream(screen)

    def read(seconds: float = 0.25) -> str:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select(
                [master], [], [], min(0.05, max(0, deadline - time.monotonic()))
            )
            if not ready:
                continue
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            raw.extend(data)
            stream.feed(data)
        return "\n".join(screen.display)

    def send(data: bytes) -> None:
        os.write(master, data)

    def capture(name: str) -> str:
        visible = read(0.45)
        (output / f"{name}.txt").write_text(visible)
        render_screen(screen, output / f"{name}.png")
        captures.append(name)
        return visible

    blocker = None
    try:
        visible = read(5)
        deadline = time.monotonic() + 20
        while not any(
            marker in visible.lower()
            for marker in ("shift+tab to cycle", "manual mode on")
        ):
            lower = visible.lower()
            if "detected a custom api key" in lower and "want to use" in lower:
                send(b"\x1b[A\r")
            elif "security notes:" in lower and "press enter to continue" in lower:
                send(b"\r")
            elif "trust" in lower and "folder" in lower:
                send(b"\x1b[B\r")
            elif "theme" in lower and "choose" in lower:
                send(b"\r")
            elif process.poll() is not None:
                raise RuntimeError(f"Claude exited during startup: {process.returncode}")
            elif time.monotonic() >= deadline:
                raise TimeoutError(f"timed out reaching the composer:\n{visible}")
            visible = read(1)
        capture("00-ready")
        actions(send, read, capture)
    except Exception as error:
        blocker = f"{type(error).__name__}: {error}"
        capture("blocked")
    finally:
        (output / "terminal.ansi").write_bytes(raw)
        if process.poll() is None:
            try:
                process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
    return {"captures": captures, "blocker": blocker}


def run(output: Path, *, columns: int, rows: int) -> dict[str, object]:
    executable_name = shutil.which("claude")
    if executable_name is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    executable = Path(executable_name).resolve()
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    server = LoopbackServer(("127.0.0.1", 0))
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    scenarios: dict[str, dict[str, object]] = {}
    artifact_paths: list[str] = []
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-hooks-init-reference-") as folder:
            root = Path(folder)
            for part in ("home", "config", "workspace"):
                (root / part).mkdir()
            (root / "config/.claude.json").write_text(
                json.dumps(
                    {
                        "hasCompletedOnboarding": True,
                        "theme": "dark",
                        "lastOnboardingVersion": "2.1.269",
                    }
                )
            )
            project = root / "workspace/.claude"
            project.mkdir()
            (project / "settings.json").write_text(
                json.dumps(
                    {
                        "hooks": {
                            "PostToolUse": [
                                {
                                    "matcher": "Edit|Write",
                                    "hooks": [
                                        {
                                            "type": "command",
                                            "command": "printf fixture-hook",
                                        }
                                    ],
                                }
                            ],
                            "Stop": [
                                {
                                    "hooks": [
                                        {
                                            "type": "prompt",
                                            "prompt": "Return ok for this inert fixture.",
                                        }
                                    ]
                                }
                            ],
                        }
                    },
                    indent=2,
                )
            )
            environment = sanitized_environment(root, server.server_port)

            def hooks_actions(send, read, capture):
                send(b"/hooks")
                read(0.35)
                capture("01-hooks-prefix")
                send(b"\r")
                read(1.2)
                capture("02-hooks-events")
                send(b"\x1b[B\r")
                read(0.7)
                capture("03-hooks-event-detail")
                send(b"\r")
                read(0.7)
                capture("04-hooks-handler-detail")

            scenarios["hooks"] = run_child(
                executable,
                output / "hooks",
                root,
                environment,
                hooks_actions,
                columns=columns,
                rows=rows,
            )

            def inspector_actions(send, read, capture):
                send(b"/tools")
                read(0.35)
                capture("01-tools-prefix")
                send(b"\x15/context")
                read(0.35)
                capture("02-context-prefix")

            scenarios["inspector"] = run_child(
                executable,
                output / "inspector",
                root,
                environment,
                inspector_actions,
                columns=columns,
                rows=rows,
            )

            environment["CLAUDE_CODE_NEW_INIT"] = "1"

            def init_actions(send, read, capture):
                send(b"/init")
                read(0.35)
                capture("01-init-prefix")

            scenarios["init"] = run_child(
                executable,
                output / "init",
                root,
                environment,
                init_actions,
                columns=columns,
                rows=rows,
            )
            artifact_paths = sorted(
                str(path.relative_to(root / "workspace"))
                for path in (root / "workspace").rglob("*")
                if path.is_file() and path != project / "settings.json"
            )
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)

    version = subprocess.run(
        [str(executable), "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    message_requests = [
        request for request in server.requests if request["path"].startswith("/v1/messages?")
    ]
    result: dict[str, object] = {
        "status": "passed"
        if all(scenario["blocker"] is None for scenario in scenarios.values())
        and not message_requests
        and not artifact_paths
        else "failed",
        "claude_version": version,
        "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "viewport": [columns, rows],
        "scenarios": scenarios,
        "model_prompt_submitted": False,
        "message_inference_requests": message_requests,
        "loopback_requests": server.requests,
        "project_artifacts_created": artifact_paths,
        "disposable_home_config_workspace": True,
        "credential_source": "dummy loopback key only",
    }
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tmp/terminal-evidence/hooks-init-claude-reference-20260911T193704Z-6250e7"),
    )
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=45)
    arguments = parser.parse_args()
    run(arguments.output, columns=arguments.columns, rows=arguments.rows)
