#!/usr/bin/env python3
"""Drive heycode through the core Read/Write/Edit/Bash/Glob/Grep card states.

The journey mirrors ``scripts/core_tool_cards_claude_reference_pty.py`` so the
two capture sets can be compared state by state: approval dialog, running,
completed, grouped/collapsed, expanded, empty result, non-zero exit, long
bounded output and a denied request.  Every model response comes from a
loopback OpenRouter-shaped fixture on 127.0.0.1; no commercial provider is
contacted.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
import threading
import time
from pathlib import Path
from tempfile import TemporaryDirectory

import pyte

from plan_review_pty import MODEL
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui, plain

LONG_OUTPUT_LINES = 40
FINAL_TEXT = "CORE_TOOL_CARDS_DONE"

#: One entry per fixture tool call.  ``capture`` names the approval screenshot
#: and ``answer`` says how the operator responds to it.
STEPS: list[dict[str, object]] = [
    {"capture": "read", "answer": "yes", "name": "read"},
    {"capture": "write", "answer": "yes", "name": "write"},
    {"capture": "edit", "answer": "yes", "name": "edit"},
    {"capture": "bash-slow", "answer": "yes", "name": "bash", "running": True},
    {"capture": "glob", "answer": "yes", "name": "glob"},
    {"capture": "grep", "answer": "yes", "name": "grep"},
    {"capture": "grep-empty", "answer": "yes", "name": "grep"},
    {"capture": "bash-failing", "answer": "yes", "name": "bash"},
    {"capture": "bash-long", "answer": "yes", "name": "bash"},
    {"capture": "bash-denied", "answer": "no", "name": "bash"},
]


def _arguments(step: int, results: dict[str, object]) -> tuple[str, dict[str, object]]:
    if step == 0:
        return "read", {"path": "sample.txt"}
    if step == 1:
        return "write", {
            "path": "notes.txt",
            "content": "first note\nsecond note\nthird note\n",
        }
    if step == 2:
        revision = results["core-0"]["revision"]
        return "edit", {
            "path": "sample.txt",
            "old_string": "beta",
            "new_string": "delta",
            "expected_revision": revision,
        }
    if step == 3:
        return "bash", {"command": "sleep 2; printf 'CORE_TOOLS_SLOW_DONE\\n'"}
    if step == 4:
        return "glob", {"pattern": "*.txt"}
    if step == 5:
        return "grep", {"pattern": "delta"}
    if step == 6:
        return "grep", {"pattern": "no-such-token-anywhere"}
    if step == 7:
        return "bash", {"command": "printf 'core tools failure\\n' >&2; exit 3"}
    if step == 8:
        return "bash", {
            "command": (
                "for i in $(seq 1 %d); do printf 'CORE_OUTPUT_LINE_%%s\\n' \"$i\"; done"
                % LONG_OUTPUT_LINES
            )
        }
    if step == 9:
        return "bash", {"command": "printf 'must not run\\n' > denied.txt"}
    return "", {}


def journey(binary: Path, output: Path, *, columns: int, rows: int, color: bool, theme: str = "heycode-dark"):
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []
    results: dict[str, object] = {}
    errors: list[str] = []
    captures: list[str] = []
    with TemporaryDirectory(prefix="heycode-core-tools-", dir="/tmp") as temporary:
        root = Path(temporary)
        home = root / "home"
        work = root / "work"
        home.mkdir()
        work.mkdir()
        (work / "sample.txt").write_text("alpha\nbeta\ngamma\n")
        (work / "other.txt").write_text("delta again\n")

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler API
                payload = (
                    {"data": {"label": "fixture"}}
                    if self.path.endswith("/key")
                    else (
                        {"data": MODEL}
                        if "/model/" in self.path
                        else {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                    )
                )
                body = json.dumps(payload).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler API
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                for message in request["messages"]:
                    if message.get("role") == "tool":
                        content = message.get("content", "")
                        try:
                            results[message["tool_call_id"]] = json.loads(content)
                        except (TypeError, ValueError):
                            results[message["tool_call_id"]] = content
                step = len(requests) - 1
                try:
                    name, arguments = _arguments(step, results)
                except Exception as error:  # noqa: BLE001 - reported in result.json
                    errors.append(repr(error))
                    name, arguments = "", {}
                if name:
                    delta = {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": f"core-{step}",
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": json.dumps(arguments),
                                },
                            }
                        ],
                        "reasoning_details": [
                            {
                                "type": "reasoning.text",
                                "text": "Exercise the core tool card states.",
                            }
                        ],
                    }
                    finish = "tool_calls"
                else:
                    delta = {"content": FINAL_TEXT}
                    finish = "stop"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for content, reason in [(delta, None), ({}, finish)]:
                    chunk = {
                        "id": "core",
                        "object": "chat.completion.chunk",
                        "model": MODEL["id"],
                        "choices": [
                            {"index": 0, "delta": content, "finish_reason": reason}
                        ],
                    }
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "settings.toml").write_text(
            f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n'
        )
        (home / "config.toml").write_text(
            f'schema_version = 31\n[llm]\nprovider="openrouter"\nmodel="{MODEL["id"]}"\n'
            f'api_key_env="HEYCODE_CORE_TOOLS_FIXTURE"\nbase_url="{base}"\n'
        )
        os.environ["HEYCODE_CORE_TOOLS_FIXTURE"] = "fixture-key"
        tui = FullScreenTui(
            str(home),
            str(work),
            str(binary),
            fake=False,
            color=color,
            rows=rows,
            columns=columns,
            extra=[
                "--provider",
                "openrouter",
                "--model",
                MODEL["id"],
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_CORE_TOOLS_FIXTURE",
            ],
        )

        class Screen(pyte.Screen):
            def set_mode(self, *modes, **kwargs):
                if kwargs.get("private") and 1049 in modes:
                    self.reset()
                return super().set_mode(*modes, **kwargs)

        screen = Screen(columns, rows)
        stream = TerminalByteStream(screen)
        seen = ""

        def render_capture(path: Path) -> None:
            render_screen(
                screen, path,
                foreground="#20242c" if theme == "heycode-light" else "#dddddd",
                background="#f8f9fb" if theme == "heycode-light" else "#101014",
            )

        def read(seconds: float = 0.15) -> str:
            nonlocal seen
            data = tui.read(seconds)
            stream.feed(data)
            seen += plain(data)
            return "\n".join(screen.display)

        def send(data: bytes) -> None:
            os.write(tui.fd, data)
            read(0.2)

        def wait(text: str, seconds: float = 40) -> str:
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                current = read()
                if text in current or text in seen:
                    return current
                if errors:
                    raise AssertionError(errors)
            raise AssertionError(f"missing {text}: {seen[-3000:]}")

        def capture(name: str, settle: float = 0.35) -> str:
            current = read(settle)
            (output / f"{name}.txt").write_text(current)
            render_capture(output / f"{name}.png")
            captures.append(name)
            return current

        result: dict[str, object] = {
            "status": "failed",
            "engine": "heycode",
            "external_provider": False,
            "external_endpoints": [],
            "viewport": f"{columns}x{rows}",
            "theme": theme,
            "color": color,
            "binary": str(binary),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        }
        try:
            # The footer wording is owned elsewhere, so the readiness gate is
            # the header the shell always prints.
            wait("for shortcuts")
            read(0.6)
            capture("01-ready")
            send(b"Exercise the core file and shell tools\r")
            answered: list[dict[str, str]] = []
            slow_running_captured = False
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                current = read(0.25)
                if FINAL_TEXT in current:
                    break
                if errors:
                    raise AssertionError(errors)
                if "Tab to amend" not in current:
                    if (
                        not slow_running_captured
                        and "sleep 2" in current
                        and "Tab to amend" not in current
                    ):
                        (output / "bash-slow-running.txt").write_text(current)
                        render_capture(output / "bash-slow-running.png")
                        captures.append("bash-slow-running")
                        slow_running_captured = True
                    continue
                index = len(answered)
                step = STEPS[index] if index < len(STEPS) else {"capture": f"extra-{index}", "answer": "yes"}
                label = f"{index + 2:02d}-approval-{step['capture']}"
                capture(label)
                if step["answer"] == "yes":
                    send(b"y")
                else:
                    send(b"n")
                answered.append({"capture": label, "action": str(step["answer"])})
            wait(FINAL_TEXT)
            time.sleep(0.5)
            completed = capture("90-completed", settle=0.6)
            assert not (work / "denied.txt").exists(), "denied Bash must not run"
            assert (work / "sample.txt").read_text() == "alpha\ndelta\ngamma\n"
            assert (work / "notes.txt").exists()

            # Global verbose mode must reveal every retained tool group,
            # protect the hidden draft and avoid any provider/session writes.
            journals = {str(path): path.read_bytes() for path in home.rglob("session.jsonl")}
            request_count = len(requests)
            send(b"UNSENT_VERBOSE_DRAFT")
            capture("90-unsent-draft")
            send(b"\x0f")
            detailed = capture("91-expanded")
            assert "Showing detailed transcript" in detailed, detailed
            assert "UNSENT_VERBOSE_DRAFT" not in detailed, detailed
            send(b"?")
            help_text = capture("91-expanded-shortcuts")
            assert "oldest/newest" in help_text and "esc close" in help_text, help_text
            send(b"\x7f")
            assert "oldest/newest" not in capture("91-expanded-shortcuts-dismissed")
            send(b"\x1b[200~/quit\n\x1b[201~")
            send(b"\r")
            for page in range(1, 4):
                send(b"\x1b[5~")
                capture(f"91-expanded-scrollback-{page}", settle=0.35)
            send(b"\x0f")
            collapsed = capture("92-collapsed")
            assert "Showing detailed transcript" not in collapsed, collapsed
            assert "UNSENT_VERBOSE_DRAFT" in collapsed, collapsed
            assert len(requests) == request_count, "verbose navigation submitted a model request"
            assert {str(path): path.read_bytes() for path in home.rglob("session.jsonl")} == journals, "verbose navigation changed journal bytes"

            events = [
                json.loads(line)
                for log in home.rglob("session.jsonl")
                for line in log.read_text().splitlines()
            ]
            (output / "events.json").write_text(json.dumps(events, indent=2))
            result.update(
                {
                    "status": "captured",
                    "provider_requests": len(requests),
                    "approvals": answered,
                    "denied_file_absent": True,
                    "sample_after_edit": (work / "sample.txt").read_text(),
                    "completed_has_marker": FINAL_TEXT in completed,
                    "verbose_toggle_draft_and_journal_preserved": True,
                    "fixture_origin": base,
                }
            )
        except Exception as error:  # noqa: BLE001 - recorded honestly
            result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
        finally:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            (output / "fixture-requests.json").write_text(
                json.dumps([r.get("messages", [])[-1] for r in requests], indent=2)[:200000]
            )
            (output / "tool-results.json").write_text(
                json.dumps(results, indent=2, default=str)[:400000]
            )
            result["captures"] = captures
            (output / "result.json").write_text(json.dumps(result, indent=2))
            tui.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/heycode")
    parser.add_argument("--output", required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    arguments = parser.parse_args()
    print(
        json.dumps(
            journey(
                Path(arguments.binary).resolve(),
                Path(arguments.output),
                columns=arguments.columns,
                rows=arguments.rows,
                color=not arguments.no_color,
                theme=arguments.theme,
            ),
            indent=2,
        )
    )
