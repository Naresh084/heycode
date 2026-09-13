#!/usr/bin/env python3
"""Exercise source-backed native Edit approvals through the production CLI.

Every case uses a disposable workspace and a deterministic localhost provider.
The provider asks the production native runtime to Read and then Edit the exact
same file/revision.  The real PTY drives accept, reject, and Escape boundaries;
no commercial provider, external service, or user file participates.
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

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash-20260826",
    "name": "Phase 2 edit approval fixture",
    "created": 1789080000,
    "description": "Deterministic localhost-only approval fixture",
    "context_length": 1310720,
    "architecture": {
        "input_modalities": ["text"],
        "output_modalities": ["text"],
        "tokenizer": "Other",
        "instruct_type": None,
    },
    "pricing": {"prompt": "0", "completion": "0", "input_cache_read": "0"},
    "top_provider": {
        "context_length": 1048576,
        "max_completion_tokens": 131072,
        "is_moderated": False,
    },
    "supported_parameters": [
        "include_reasoning",
        "max_tokens",
        "reasoning",
        "reasoning_effort",
        "temperature",
        "tool_choice",
        "tools",
    ],
    "default_parameters": {"temperature": 1},
    "expiration_date": "2098-12-31",
    "reasoning": {
        "mandatory": True,
        "default_enabled": True,
        "supported_efforts": ["max", "high", "low"],
        "default_effort": "max",
    },
}
ORIGINAL = "alpha\nmode=slow\ngamma\n"
EDITED = "alpha\nmode=fast\ngamma\n"


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def parse_tool_results(request: dict[str, object]) -> dict[str, object]:
    results: dict[str, object] = {}
    for message in request.get("messages", []):
        if message.get("role") != "tool":
            continue
        content = message.get("content", "")
        try:
            results[str(message["tool_call_id"])] = json.loads(str(content))
        except (KeyError, TypeError, ValueError):
            results[str(message.get("tool_call_id", "unknown"))] = content
    return results


def run_case(
    binary: Path,
    output: Path,
    case: str,
    key: bytes,
    *,
    color: bool,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    requests: list[dict[str, object]] = []
    provider_errors: list[str] = []
    with TemporaryDirectory(prefix=f"heycode-edit-approval-{case}-") as temporary:
        root = Path(temporary)
        home = root / "home"
        work = root / "work"
        target = work / "src" / "config.txt"
        home.mkdir()
        target.parent.mkdir(parents=True)
        target.write_text(ORIGINAL)

        class Handler(http.server.BaseHTTPRequestHandler):
            def respond_json(self, value: object) -> None:
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                if self.path.endswith("/key"):
                    self.respond_json({"data": {"label": "terminal-local-fixture"}})
                elif "/model/" in self.path:
                    self.respond_json({"data": MODEL})
                else:
                    self.respond_json(
                        {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                    )

            def do_POST(self):
                request = json.loads(
                    self.rfile.read(int(self.headers["Content-Length"]))
                )
                requests.append(request)
                (output / f"{case}-requests.json").write_text(
                    json.dumps(requests, indent=2)
                )
                step = len(requests) - 1
                results = parse_tool_results(request)
                try:
                    if step == 0:
                        name = "read"
                        arguments = {"path": "src/config.txt"}
                        call_id = f"{case}-read"
                        final = None
                    elif step == 1:
                        read = results[f"{case}-read"]
                        assert isinstance(read, dict), read
                        assert read["path"] == "src/config.txt", read
                        assert read["content"] == (
                            "   1\talpha\n   2\tmode=slow\n   3\tgamma"
                        ), read
                        assert read["truncated"] is False, read
                        name = "edit"
                        arguments = {
                            "path": "src/config.txt",
                            "old_string": "mode=slow",
                            "new_string": "mode=fast",
                            "expected_revision": read["revision"],
                        }
                        call_id = f"{case}-edit"
                        final = None
                    elif step == 2:
                        edit_result = results[f"{case}-edit"]
                        if case == "accept":
                            assert isinstance(edit_result, dict), edit_result
                            assert edit_result["changed"] is True, edit_result
                            assert target.read_text() == EDITED
                        else:
                            assert target.read_text() == ORIGINAL
                            assert "denied" in str(edit_result).lower(), edit_result
                            if case == "amend":
                                assert "keep the original setting" in str(edit_result), edit_result
                        name = None
                        arguments = None
                        call_id = None
                        final = f"{case.upper()}_DONE"
                    else:
                        raise AssertionError(f"unexpected provider request {step}")
                except Exception as error:  # evidence keeps the exact fixture failure
                    provider_errors.append(repr(error))
                    name = None
                    arguments = None
                    call_id = None
                    final = f"FIXTURE_FAILED {error!r}"

                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if name is not None:
                    delta = {
                        "reasoning": "Use the exact native file observation and mutation boundary.",
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": json.dumps(arguments),
                                },
                            }
                        ],
                    }
                    finish = "tool_calls"
                else:
                    delta = {"content": final}
                    finish = "stop"
                for content, finish_reason in [(delta, None), ({}, finish)]:
                    chunk = {
                        "id": f"terminal-edit-{case}-{step}",
                        "object": "chat.completion.chunk",
                        "model": MODEL["id"],
                        "choices": [
                            {
                                "index": 0,
                                "delta": content,
                                "finish_reason": finish_reason,
                            }
                        ],
                    }
                    self.wfile.write(
                        ("data: " + json.dumps(chunk) + "\n\n").encode()
                    )
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "settings.toml").write_text("schema_version = 1\n")
        (home / "config.toml").write_text(
            "schema_version = 31\n"
            "[llm]\n"
            'provider = "openrouter"\n'
            f'model = "{MODEL["id"]}"\n'
            'api_key_env = "HEYCODE_EDIT_APPROVAL_FIXTURE"\n'
            f'base_url = "{base}"\n'
        )
        os.environ["HEYCODE_EDIT_APPROVAL_FIXTURE"] = "local-only-not-a-credential"
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
                "--approval",
                "default",
                "--sandbox",
                "workspace",
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_EDIT_APPROVAL_FIXTURE",
            ],
        )
        screen = Screen(columns, rows)
        stream = TerminalByteStream(screen)

        def read(seconds: float = 0.15) -> str:
            stream.feed(tui.read(seconds))
            return "\n".join(screen.display)

        def wait(needle: str, wait_timeout: float | None = None) -> str:
            deadline = time.monotonic() + (wait_timeout or timeout)
            latest = "\n".join(screen.display)
            while time.monotonic() < deadline:
                latest = read()
                if needle in latest:
                    return latest
                if provider_errors:
                    raise AssertionError(provider_errors)
                if not tui.alive():
                    raise AssertionError(
                        f"CLI exited while waiting for {needle!r}:\n{latest}"
                    )
            raise AssertionError(f"missing {needle!r}:\n{latest}")

        def send(data: bytes) -> str:
            os.write(tui.fd, data)
            return read(0.2)

        try:
            wait("? for shortcuts")
            send(b"Perform the exact observed edit in this disposable fixture\r")
            wait("Read file")
            send(b"y")
            pending = wait("Edit file")
            pending = wait("mode=fast")
            assert target.read_text() == ORIGINAL, "Edit ran before approval"
            assert "mode=slow" in pending and "mode=fast" in pending, pending
            assert "2 -mode=slow" in pending, pending
            assert "2 +mode=fast" in pending, pending
            assert "1  alpha" in pending and "3  gamma" in pending, pending
            assert "expected_revision:" not in pending, pending
            assert "old_string:" not in pending and "new_string:" not in pending, pending
            assert "1. Yes" in pending and "No" in pending, pending
            assert "Esc to cancel · Tab to amend" in pending, pending
            assert "Permission requested" not in pending, pending

            styled_rows: dict[str, dict[str, object]] = {}
            for label, token in [("removed", "-mode=slow"), ("added", "+mode=fast")]:
                row_index = next(
                    index for index, line in enumerate(screen.display) if token in line
                )
                line = screen.display[row_index]
                marker_column = line.index(token)
                number_column = line.rfind("2", 0, marker_column)
                marker = screen.buffer[row_index][marker_column]
                number = screen.buffer[row_index][number_column]
                styled_rows[label] = {
                    "row": row_index + 1,
                    "number_column": number_column + 1,
                    "marker_column": marker_column + 1,
                    "number_fg": number.fg,
                    "number_bg": number.bg,
                    "marker_fg": marker.fg,
                    "marker_bg": marker.bg,
                }
                assert number.fg == marker.fg
                assert number.bg == marker.bg
            if color:
                assert styled_rows["removed"]["marker_fg"] != styled_rows["added"][
                    "marker_fg"
                ]
                assert styled_rows["removed"]["marker_bg"] != styled_rows["added"][
                    "marker_bg"
                ]
            else:
                for style in styled_rows.values():
                    assert style["marker_fg"] == "default", style
                    assert style["marker_bg"] == "default", style

            pending_name = f"{case}-pending"
            (output / f"{pending_name}.txt").write_text(pending)
            (output / f"{pending_name}.ansi").write_bytes(tui.transcript)
            render_screen(screen, output / f"{pending_name}.png")
            (output / f"{case}-pending-file.json").write_text(
                json.dumps(
                    {
                        "sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                        "content": target.read_text(),
                        "styled_rows": styled_rows,
                        "color": color,
                    },
                    indent=2,
                )
            )

            if case == "amend":
                send(b"\t")
                amendment = wait("Amendment:")
                assert target.read_text() == ORIGINAL
                assert len(requests) == 2, "Tab must not approve or deny yet"
                (output / "amend-editor.txt").write_text(amendment)
                render_screen(screen, output / "amend-editor.png")
                send(b"keep the original setting\r")
            else:
                send(key)
            if case == "escape":
                finished = wait("Cancelled")
                assert len(requests) == 2, (
                    "Escape cancels before publishing a tool result/model request",
                    len(requests),
                )
            else:
                finished = wait(f"{case.upper()}_DONE")
                assert len(requests) == 3, len(requests)
            expected = EDITED if case == "accept" else ORIGINAL
            assert target.read_text() == expected
            assert not provider_errors, provider_errors
            (output / f"{case}-settled.txt").write_text(finished)
            (output / f"{case}-settled.ansi").write_bytes(tui.transcript)
            render_screen(screen, output / f"{case}-settled.png")
            events = [
                json.loads(line)
                for journal in home.rglob("session.jsonl")
                for line in journal.read_text().splitlines()
                if line.strip()
            ]
            (output / f"{case}-events.json").write_text(
                json.dumps(events, indent=2)
            )
            return {
                "case": case,
                "color": color,
                "requests": len(requests),
                "pending_file_sha256": hashlib.sha256(ORIGINAL.encode()).hexdigest(),
                "settled_file_sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
                "styled_rows": styled_rows,
                "events": len(events),
            }
        finally:
            tui.close()
            server.shutdown()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="tmp/cli-snapshots/25526dce041310f5/dshx")
    parser.add_argument(
        "--output", default="tmp/terminal-evidence/edit-approval-controlled-20260911T114446Z-20da68"
    )
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=35)
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    binary = Path(args.binary).resolve()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    before = hashlib.sha256(binary.read_bytes()).hexdigest()
    results = [
        run_case(
            binary,
            output,
            "accept",
            b"y",
            color=True,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        ),
        run_case(
            binary,
            output,
            "deny",
            b"n",
            color=True,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        ),
        run_case(
            binary,
            output,
            "escape",
            b"\x1b",
            color=True,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        ),
        run_case(
            binary,
            output,
            "amend",
            b"\t",
            color=True,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        ),
        run_case(
            binary,
            output,
            "monochrome",
            b"n",
            color=False,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        ),
    ]
    after = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert before == after
    result = {
        "status": "passed",
        "binary": str(binary),
        "binary_sha256": before,
        "transport": "localhost deterministic OpenAI-compatible SSE fixture",
        "commercial_provider_requests": 0,
        "cases": results,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(f"PASS: {output}")


if __name__ == "__main__":
    main()
