#!/usr/bin/env python3
"""Prove `/copy` table normalization through an immutable heycode CLI binary."""
from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import threading
import time

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


RAW = (
    "Synthetic markdown table normalization.\n\n"
    "| Item | Result |\n"
    "|:--|--:|\n"
    "| a | b |\n\n"
    "```python\n"
    'print("alpha")\n'
    "```\n\n"
    "```json\n"
    '{"ok": true}\n'
    "```"
)
EXPECTED = (
    "Synthetic markdown table normalization.\n\n"
    "| Item | Result |\n"
    "| :--- | -----: |\n"
    "| a    | b      |\n\n"
    "```python\n"
    'print("alpha")\n'
    "```\n\n"
    "```json\n"
    '{"ok": true}\n'
    "```"
)
MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Local copy table contract fixture",
    "created": 1787752741,
    "description": "Local deterministic copy table fixture",
    "context_length": 131072,
    "architecture": {
        "input_modalities": ["text"],
        "output_modalities": ["text"],
        "tokenizer": "Other",
        "instruct_type": None,
    },
    "pricing": {"prompt": "0", "completion": "0"},
    "top_provider": {
        "context_length": 131072,
        "max_completion_tokens": 32768,
        "is_moderated": False,
    },
    "supported_parameters": [
        "max_tokens",
        "temperature",
        "tool_choice",
        "tools",
        "reasoning",
    ],
    "reasoning": {
        "mandatory": True,
        "default_enabled": True,
        "supported_efforts": ["max", "high", "low"],
        "default_effort": "max",
    },
}
OSC52 = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]+)\x07")


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, output: Path, *, timeout: float) -> dict[str, object]:
    binary = binary.resolve()
    if not binary.is_file():
        raise FileNotFoundError(binary)
    output.mkdir(parents=True, exist_ok=False)
    requests: list[dict[str, object]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path.endswith("/key"):
                self.respond({"data": {"label": "copy-table-local-fixture"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            payload = {
                "id": "copy-table-fixture-1",
                "object": "chat.completion.chunk",
                "model": MODEL["id"],
                "choices": [
                    {"index": 0, "delta": {"content": RAW}, "finish_reason": None}
                ],
            }
            finish = {
                "id": "copy-table-fixture-1",
                "object": "chat.completion.chunk",
                "model": MODEL["id"],
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            }
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.write(("data: " + json.dumps(finish) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    previous = {
        name: os.environ.get(name)
        for name in ("HEYCODE_COPY_TABLE_FIXTURE", "SSH_CONNECTION", "TMPDIR")
    }
    tui: FullScreenTui | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-copy-table-pty-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            copy_temp = root / "copy-temp"
            home.mkdir()
            workspace.mkdir()
            copy_temp.mkdir()
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "settings.toml").write_text(
                'schema_version = 1\n[settings.ui-preferences]\ncopy_full_response = false\n'
            )
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_COPY_TABLE_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            os.environ["HEYCODE_COPY_TABLE_FIXTURE"] = "local-only-not-a-real-credential"
            os.environ["SSH_CONNECTION"] = "127.0.0.1 fixture 127.0.0.1 fixture"
            os.environ["TMPDIR"] = str(copy_temp)

            tui = FullScreenTui(
                str(home),
                str(workspace),
                str(binary),
                fake=False,
                rows=44,
                columns=110,
                color=True,
                extra=[
                    "--provider",
                    "openrouter",
                    "--model",
                    MODEL["id"],
                    "--approval",
                    "full_access",
                    "--set",
                    f"llm.base_url={base}",
                    "--set",
                    "llm.api_key_env=HEYCODE_COPY_TABLE_FIXTURE",
                ],
            )
            screen = Screen(110, 44)
            stream = TerminalByteStream(screen)

            def read(seconds: float = 0.12) -> str:
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def send(value: bytes) -> None:
                os.write(tui.fd, value)
                read(0.15)

            def wait_for(*needles: str) -> str:
                deadline = time.monotonic() + timeout
                latest = ""
                while time.monotonic() < deadline:
                    latest = read()
                    if all(needle in latest for needle in needles):
                        return latest
                    if not tui.alive():
                        raise AssertionError(
                            f"CLI exited while waiting for {needles!r}:\n{latest}"
                        )
                raise AssertionError(f"timed out waiting for {needles!r}:\n{latest}")

            def capture(name: str) -> None:
                visible = read(0.4)
                (output / f"{name}.txt").write_text(visible)
                (output / f"{name}.ansi").write_bytes(tui.transcript)
                render_screen(screen, output / f"{name}.png")

            wait_for("Full access")
            send(b"COPY_TABLE_FIXTURE\r")
            wait_for("Synthetic markdown table normalization.", '{"ok": true}')
            capture("01-raw-table-answer")
            assert len(requests) == 1, requests

            send(b"/copy\r")
            picker = wait_for("Select content to copy:", "Full response")
            assert "Always copy full response" in picker, picker
            capture("02-full-response-selected")
            send(b"\r")
            copied = wait_for("Copied to clipboard")
            assert "150 characters" in copied, copied
            capture("03-normalized-copy-result")

            payloads = [
                base64.b64decode(value).decode()
                for value in OSC52.findall(tui.transcript)
            ]
            assert payloads == [EXPECTED], payloads
            recovery_files = [
                path
                for path in copy_temp.rglob("response.md")
                if path.parent.name.startswith("heycode-copy-")
            ]
            assert len(recovery_files) == 1, recovery_files
            recovery = recovery_files[0]
            assert recovery.read_text() == EXPECTED
            assert stat.S_IMODE(recovery.parent.stat().st_mode) == 0o700
            assert stat.S_IMODE(recovery.stat().st_mode) == 0o600

            events: list[dict[str, object]] = []
            for path in sorted(home.rglob("session.jsonl")):
                events.extend(
                    json.loads(line)
                    for line in path.read_text().splitlines()
                    if line.strip()
                )
            user_messages = [
                event.get("data", {}).get("text")
                for event in events
                if event.get("kind") == "user/message"
            ]
            assert user_messages == ["COPY_TABLE_FIXTURE"], user_messages
            (output / "events.json").write_text(json.dumps(events, indent=2))
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            (output / "raw.txt").write_text(RAW)
            (output / "expected.txt").write_text(EXPECTED)
            (output / "osc52-payloads.json").write_text(json.dumps(payloads, indent=2))
            (output / "recovery-evidence.json").write_text(
                json.dumps(
                    {
                        "filename": recovery.name,
                        "sha256": hashlib.sha256(recovery.read_bytes()).hexdigest(),
                        "utf8": recovery.read_text(),
                        "directory_mode": "0700",
                        "file_mode": "0600",
                    },
                    indent=2,
                )
            )

        result: dict[str, object] = {
            "status": "passed",
            "binary": str(binary),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "runtime": "real native heycode CLI full-screen TUI over PTY",
            "transport": "localhost deterministic SSE fixture",
            "external_provider_requests": 0,
            "provider_requests": len(requests),
            "source_bytes": len(RAW.encode()),
            "normalized_bytes": len(EXPECTED.encode()),
            "osc52_payload_exact": True,
            "private_recovery_exact": True,
            "markdown_table_normalization_exact": True,
            "fenced_regions_preserved_exactly": True,
            "slash_command_in_model_input": False,
            "captures": [
                "01-raw-table-answer",
                "02-full-response-selected",
                "03-normalized-copy-result",
            ],
            "evidence_boundary": (
                "Controlled local CLI/PTy acceptance on the recorded immutable binary; "
                "the matching Claude source fixture is retained separately."
            ),
        }
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        return result
    finally:
        if tui is not None:
            tui.close()
        for name, value in previous.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    run(args.binary, args.output.resolve(), timeout=args.timeout)
