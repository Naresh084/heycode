#!/usr/bin/env python3
"""Capture current Claude Code `/advisor` source semantics without inference.

The process receives a disposable HOME, config directory, workspace and
session. Credentials are scrubbed, the only key is a dummy value, and the
Anthropic base URL is a loopback fixture that records and rejects provider
requests. Claude's documented experimental-advisor environment override is
enabled so the packaged command can be inspected independently of account
rollout. No conversation prompt is submitted.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import uuid

from claude_compaction_terminal_reference import ClaudePty, sha256


def run(output: Path, *, columns: int, rows: int) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    requests: list[dict[str, object]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            requests.append({"method": "GET", "path": self.path, "loopback": True})
            body = b"{}"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body_value = json.loads(raw or b"{}")
            requests.append(
                {
                    "method": "POST",
                    "path": self.path,
                    "loopback": True,
                    "request_bytes": len(raw),
                    "model": body_value.get("model"),
                    "stream": body_value.get("stream"),
                    "max_tokens": body_value.get("max_tokens"),
                }
            )
            body = json.dumps(
                {
                    "id": "msg_local_advisor_validation",
                    "type": "message",
                    "role": "assistant",
                    "model": body_value.get("model", "claude-opus-5-local"),
                    "content": [{"type": "text", "text": "LOCAL_MODEL_VALIDATION"}],
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    captures: list[str] = []
    blocker: str | None = None
    session_id = str(uuid.uuid4())

    try:
        with tempfile.TemporaryDirectory(prefix="claude-advisor-reference-") as folder:
            root = Path(folder)
            home = root / "home"
            config = root / "config"
            workspace = root / "workspace"
            for path in (home, config, workspace):
                path.mkdir()
            (config / ".claude.json").write_text(
                json.dumps(
                    {
                        "hasCompletedOnboarding": True,
                        "theme": "dark",
                        "lastOnboardingVersion": "2.1.268",
                    }
                )
            )
            environment = os.environ.copy()
            for key in list(environment):
                if (
                    key.startswith(
                        ("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")
                    )
                    or key.endswith("_API_KEY")
                    or key.endswith("_AUTH_TOKEN")
                ):
                    environment.pop(key)
            environment.update(
                {
                    "HOME": str(home),
                    "TERM": "xterm-256color",
                    "COLORTERM": "truecolor",
                    "CLAUDE_CONFIG_DIR": str(config),
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                    "CLAUDE_CODE_REMOTE_CONTROL": "0",
                    "CLAUDE_CODE_NO_FLICKER": "1",
                    "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL": "1",
                    "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                }
            )
            common = [
                "--strict-mcp-config",
                "--no-chrome",
                "--setting-sources",
                "user,project,local",
                "--settings",
                '{"remoteControlAtStartup":false}',
                "--permission-mode",
                "manual",
                "--model",
                "opus",
            ]
            terminal = ClaudePty(
                executable,
                environment,
                workspace,
                [*common, "--session-id", session_id, "--name", "advisor-reference"],
                columns=columns,
                rows=rows,
            )
            try:
                terminal.ready()
                terminal.capture(output, "00-ready")
                captures.append("00-ready")
                terminal.send(b"/advisor")
                terminal.capture(output, "01-command-entered")
                captures.append("01-command-entered")
                terminal.send(b"\r")
                terminal.read(1.5)
                terminal.capture(output, "02-after-first")
                captures.append("02-after-first")
                terminal.send(b"\x1b")
                terminal.read(0.3)
                terminal.send(b"/advisor off\r")
                terminal.read(1.5)
                terminal.capture(output, "03-disabled")
                captures.append("03-disabled")
                terminal.send(b"/advisor opus\r")
                terminal.read(1.5)
                terminal.capture(output, "04-enabled-opus")
                captures.append("04-enabled-opus")
            finally:
                (output / "terminal.ansi").write_bytes(terminal.transcript)
                terminal.close()

            restart_id = str(uuid.uuid4())
            restart = ClaudePty(
                executable,
                environment,
                workspace,
                [*common, "--session-id", restart_id, "--name", "advisor-restart"],
                columns=columns,
                rows=rows,
            )
            try:
                restart.ready()
                restart.send(b"/advisor\r")
                restart.read(1.5)
                restarted = restart.capture(output, "05-restart-persisted")
                captures.append("05-restart-persisted")
                if "❯ 2. Opus 5 ✔" not in restarted:
                    raise AssertionError("restart did not show the persisted Opus advisor")
            finally:
                (output / "restart.ansi").write_bytes(restart.transcript)
                restart.close()
    except Exception as error:  # preserve captures for diagnosis
        blocker = f"{type(error).__name__}: {error}"
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)

    result = {
        "status": "blocked" if blocker else "captured",
        "blocker": blocker,
        "scope": "Claude Code /advisor local-command source behavior",
        "captures": captures,
        "claude_version": subprocess.run(
            [executable, "--version"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "claude_binary": str(Path(executable).resolve()),
        "claude_binary_sha256": sha256(Path(executable).resolve()),
        "provider_host": "127.0.0.1",
        "provider_requests": requests,
        "external_provider_requests": 0,
        "normal_conversation_prompts": 0,
        "credential_source": "dummy loopback key only",
        "session_id": session_id,
    }
    (output / "requests.json").write_text(json.dumps(requests, indent=2))
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    args = parser.parse_args()
    outcome = run(args.output, columns=args.columns, rows=args.rows)
    raise SystemExit(0 if outcome["status"] == "captured" else 1)
