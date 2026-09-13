#!/usr/bin/env python3
"""Deterministic production-binary Plan review journeys on real PTYs.

Uses only a loopback OpenRouter-shaped fixture and fresh temporary homes. No paid
provider request, installed subscription runtime, real credentials, or user files.
Run after cargo build -p heycode-cli --bin heycode:
  python3 scripts/plan_review_pty.py --binary target/debug/heycode
"""
from __future__ import annotations
import argparse
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time

from tui_blackbox import FullScreenTui, plain, shows

MODEL = {
    "id": "z-ai/glm-5.3-flash", "canonical_slug": "z-ai/glm-5.3-flash-20260826",
    "name": "Plan fixture model", "created": 1787752741, "description": "local fixture",
    "context_length": 1310720,
    "architecture": {"input_modalities": ["text"], "output_modalities": ["text"], "tokenizer": "Other", "instruct_type": None},
    "pricing": {"prompt": "0", "completion": "0", "input_cache_read": "0"},
    "top_provider": {"context_length": 1048576, "max_completion_tokens": 131072, "is_moderated": False},
    "supported_parameters": ["include_reasoning", "max_tokens", "reasoning", "reasoning_effort", "temperature", "tool_choice", "tools"],
    "default_parameters": {"temperature": 1}, "expiration_date": "2098-12-31",
    "reasoning": {"mandatory": True, "default_enabled": True, "supported_efforts": ["max", "high", "low"], "default_effort": "max"},
}
DOCUMENT = "\n".join([
    "# Full implementation plan", "## Objective", "Create one fixture marker after human acceptance.",
    "## Proposed changes", "Write the marker in this isolated temporary workspace.",
    "## Affected areas", "Only the fixture marker.", "## Implementation steps",
    *[f"{i}. Inspect and validate detailed implementation step {i}." for i in range(1, 701)],
    "## Assumptions", "The selected permission policy remains authoritative.",
    "## Risks", "Rejection, Escape and failed transitions must stay read-only.",
    "## Validation", "Confirm policy before writing and verify the marker afterwards.",
    "UNIQUE-FULL-PLAN-TAIL",
])


def journey(binary: str, output: Path, choice: str) -> dict:
    requests: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="heycode-plan-pty-") as root:
        root = Path(root)
        home, workspace = root / "home", root / "workspace"
        home.mkdir(); workspace.mkdir()
        marker = workspace / "implemented.txt"

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                payload = {"data": {"label": "fixture"}} if self.path.endswith("/key") else (
                    {"data": MODEL} if "/model/" in self.path else {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                )
                body = json.dumps(payload).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                step = len(requests)
                if step == 1:
                    tool, args = "exit_plan_mode", {"plan": DOCUMENT}
                elif step == 2:
                    # Try the identical real mutation for acceptance, rejection and Escape.
                    tool, args = "bash", {"command": f"printf implemented > '{marker}'"}
                else:
                    tool = None
                delta = {"reasoning": "Deterministic Plan review fixture."}
                finish = "stop"
                if tool:
                    delta["tool_calls"] = [{"index": 0, "id": f"plan-fixture-{step}", "type": "function", "function": {"name": tool, "arguments": json.dumps(args)}}]
                    finish = "tool_calls"
                else:
                    delta["content"] = "PLAN-FIXTURE-DONE"
                self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
                for content, reason in [(delta, None), ({}, finish)]:
                    chunk = {"id": "plan-fixture", "object": "chat.completion.chunk", "model": MODEL["id"], "choices": [{"index": 0, "delta": content, "finish_reason": reason}]}
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode()); self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "settings.toml").write_text("schema_version = 1\n")
        (home / "config.toml").write_text(f'schema_version = 30\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_PLAN_PTY_FIXTURE"\nbase_url = "{base}"\n')
        tui = FullScreenTui(str(home), str(workspace), binary, fake=False, extra=["--provider", "openrouter", "--model", MODEL["id"], "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_PLAN_PTY_FIXTURE"])
        seen = ""

        def wait_for(text: str, timeout: float = 20):
            nonlocal seen
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                seen += plain(tui.read(.1))
                if shows(seen, text):
                    return
                if not tui.alive():
                    raise AssertionError(f"binary exited waiting for {text}: {seen[-1800:]}")
            raise AssertionError(f"missing {text}: {seen[-2400:]}")

        def send(data: bytes):
            nonlocal seen
            seen = ""
            os.write(tui.fd, data)

        try:
            wait_for("Welcome to heycode")
            send(b"\x1b[B\x1b[B\r"); wait_for("Select a provider")
            send(b"OpenRouter\r"); wait_for("Paste your OpenRouter API key")
            send(b"plan-fixture-key\r"); wait_for("Choose a model")
            send(b"\r"); wait_for("/ commands")
            send(b"/permissions full_access\r"); wait_for("Full access")
            # Enter Plan through the actual mode picker, not the backend API.
            send(b"/permissions\r"); wait_for("Inspect and plan")
            send(b"\x1b[B\x1b[B\x1b[B\r"); wait_for("Plan")
            send(b"PLAN-FIXTURE-START\r"); wait_for("Plan review")
            assert not marker.exists()
            first = tui.resize(40, 111)
            assert shows(first, "Full implementation plan") and shows(first, "Default permissions"), first
            (output / f"{choice}-review-first.txt").write_text(first)
            send(b"\x1b[4~")
            tail = tui.resize(40, 110)
            assert shows(tail, "UNIQUE-FULL-PLAN-TAIL"), tail
            (output / f"{choice}-review-tail.txt").write_text(tail)
            if choice == "accepted_edits":
                send(b"\t\r")
            elif choice == "default":
                send(b"\x1b[D\r")
            elif choice == "manual":
                send(b"\x1b[Z")
            elif choice == "feedback":
                send(b"Keep files untouched\r")
            else:
                send(b"\x1b")
            if choice in ("accepted_edits", "default"):
                wait_for("Permission requested")
                assert not marker.exists(), "mutation ran before the newly selected policy asked"
                send(b"y")
            wait_for("PLAN-FIXTURE-DONE")
            assert marker.exists() == (choice in ("accepted_edits", "default", "manual"))
            logs = sorted(home.rglob("session.jsonl"))
            events = [json.loads(line) for log in logs for line in log.read_text().splitlines()]
            reviews = [event for event in events if event["kind"] == "plan/review"]
            assert len(reviews) == (1 if choice == "manual" else 2), reviews
            record = reviews[-1]["data"]
            expected = "pending" if choice == "manual" else (choice if choice in ("accepted_edits", "default") else "stay_in_plan")
            assert record["decision"] == expected, record
            assert record["plan"] == DOCUMENT, "durable review truncated the document"
            if choice == "feedback":
                assert record["feedback"] == "Keep files untouched", record
            (output / f"{choice}-events.json").write_text(json.dumps(events, indent=2))
            (output / f"{choice}-requests.json").write_text(json.dumps(requests, indent=2))
            return {"choice": choice, "durable_decision": expected, "marker_written": marker.exists(), "document_bytes": len(DOCUMENT.encode()), "provider_requests": len(requests)}
        finally:
            (output / f"{choice}-terminal.ansi").write_bytes(tui.transcript)
            tui.close(); server.shutdown(); server.server_close(); server_thread.join(timeout=2)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/heycode")
    parser.add_argument("--output", default="/tmp/dshx-plan-review-pty")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    output = Path(args.output).resolve(); output.mkdir(parents=True, exist_ok=True)
    results = [journey(binary, output, choice) for choice in ("accepted_edits", "default", "feedback", "escape", "manual")]
    (output / "result.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))

if __name__ == "__main__":
    main()
