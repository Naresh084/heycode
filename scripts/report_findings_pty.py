#!/usr/bin/env python3
"""Validate ReportFindings through the production CLI, native reviewer, and PTY.

The only inference transport is a deterministic loopback OpenRouter-shaped SSE
fixture. The script creates disposable heycode state and source files, drives the
real reviewer preset through its advertised ``read`` and ``report_findings``
tools, captures approval/completed/replay screens, preserves the optional
reference-compatible review dimensions, proves a stale read revision commits
no second report, and replays an old-shape unannotated journal without a
provider request. No provider account or external endpoint is used.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import http.server
import json
import os
import re
import shutil
import struct
import tempfile
import termios
import threading
import time
from pathlib import Path
from typing import Any

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Fixture: ReportFindings reviewer",
    "created": 1787752741,
    "description": "Deterministic localhost-only validation fixture",
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

PARENT_VALID = "REPORT_FINDINGS_PTY_VALID"
PARENT_STALE = "REPORT_FINDINGS_PTY_STALE"
CHILD_VALID = "REPORT_FINDINGS_VALID_CHILD"
CHILD_STALE = "REPORT_FINDINGS_STALE_CHILD"
STALE_ERROR = "a finding file changed since it was read; read it again before reporting"


class Screen(pyte.Screen):
    """Reset pyte when the production TUI enters its alternate screen."""

    def set_mode(self, *modes: int, **kwargs: Any) -> Any:
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def tool_call(
    name: str,
    arguments: dict[str, object],
    request_index: int,
) -> dict[str, object]:
    return {
        "index": 0,
        "id": f"report-findings-{name}-{request_index}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments)},
    }


def messages_after_last_user(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    start = 0
    for index, message in enumerate(messages):
        if message.get("role") == "user" and not str(
            message.get("content", "")
        ).startswith("[job "):
            start = index + 1
    return messages[start:]


def latest_tool_json(messages: list[dict[str, Any]]) -> dict[str, Any] | None:
    for message in reversed(messages):
        if message.get("role") != "tool":
            continue
        content = message.get("content")
        if not isinstance(content, str):
            return None
        try:
            value = json.loads(content)
        except json.JSONDecodeError:
            return None
        if isinstance(value, dict):
            return value
        return None
    return None


def run(
    binary: Path,
    output: Path,
    *,
    theme: str,
    color: bool,
    columns: int,
    rows: int,
) -> dict[str, object]:
    binary = binary.resolve()
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, Any]] = []
    request_lock = threading.Lock()
    source_root: dict[str, Path] = {}
    revisions: dict[str, str] = {}
    reviewer_catalogs: dict[str, list[str]] = {}
    reviewer_schemas: dict[str, dict[str, Any]] = {}
    stale_mutated = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond_json(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            if self.path.endswith("/key"):
                self.respond_json({"data": {"label": "report-findings-loopback"}})
            elif "/model/" in self.path:
                self.respond_json({"data": MODEL})
            else:
                self.respond_json(
                    {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                )

        def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            length = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(length))
            with request_lock:
                requests.append(request)
                request_index = len(requests)
                (output / "requests.json").write_text(
                    json.dumps(requests, indent=2, ensure_ascii=False)
                )

            messages = request.get("messages", [])
            assert isinstance(messages, list)
            user_text = [
                str(message.get("content", ""))
                for message in messages
                if message.get("role") == "user"
                and not str(message.get("content", "")).startswith("[job ")
            ]
            last_user = user_text[-1] if user_text else ""
            joined_users = "\n".join(user_text)
            tail = messages_after_last_user(messages)
            tool_results = [message for message in tail if message.get("role") == "tool"]
            tools = [
                str(tool.get("function", {}).get("name", ""))
                for tool in request.get("tools", [])
            ]

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta: dict[str, object], finish: str | None = None) -> None:
                if "tool_calls" in delta:
                    delta["reasoning"] = (
                        "Using the production native reviewer and exact source evidence."
                    )
                payload = {
                    "id": f"report-findings-pty-{request_index}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {"index": 0, "delta": delta, "finish_reason": finish}
                    ],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                self.wfile.flush()

            def call(name: str, arguments: dict[str, object]) -> None:
                chunk({"tool_calls": [tool_call(name, arguments, request_index)]})
                chunk({}, "tool_calls")

            try:
                if CHILD_VALID in joined_users or CHILD_STALE in joined_users:
                    scenario = "valid" if CHILD_VALID in joined_users else "stale"
                    reviewer_catalogs[scenario] = tools
                    assert "read" in tools, tools
                    assert "report_findings" in tools, tools
                    assert "write" not in tools, tools
                    assert "bash" not in tools, tools
                    report_tool = next(
                        tool
                        for tool in request.get("tools", [])
                        if tool.get("function", {}).get("name") == "report_findings"
                    )
                    schema = report_tool["function"]["parameters"]
                    reviewer_schemas[scenario] = schema
                    properties = schema["properties"]
                    finding_schema = properties["findings"]
                    finding_properties = finding_schema["items"]["properties"]
                    assert finding_schema["maxItems"] == 32, finding_schema
                    assert properties["level"]["enum"] == [
                        "low",
                        "medium",
                        "high",
                        "xhigh",
                        "max",
                    ], properties["level"]
                    assert finding_properties["category"]["maxLength"] == 40
                    assert finding_properties["verdict"]["enum"] == [
                        "CONFIRMED",
                        "PLAUSIBLE",
                    ]
                    assert finding_properties["outcome"]["enum"] == [
                        "fixed",
                        "skipped",
                        "no_change_needed",
                    ]
                    if not tool_results:
                        path = (
                            "review-target.rs"
                            if scenario == "valid"
                            else "stale-target.rs"
                        )
                        call("read", {"path": path})
                    else:
                        latest = latest_tool_json(tail)
                        if latest is not None and isinstance(latest.get("revision"), str):
                            revision = latest["revision"]
                            revisions[scenario] = revision
                            if scenario == "stale" and not stale_mutated.is_set():
                                (source_root["workspace"] / "stale-target.rs").write_text(
                                    "pub fn stale_flag() -> bool {\n    true\n}\n"
                                )
                                stale_mutated.set()
                            path = (
                                "review-target.rs"
                                if scenario == "valid"
                                else "stale-target.rs"
                            )
                            call(
                                "report_findings",
                                {
                                    "level": "high",
                                    "findings": [
                                        {
                                            "severity": "high",
                                            "path": path,
                                            "line_start": 2,
                                            "line_end": 2,
                                            "revision": revision,
                                            "title": "Unchecked indexing can panic",
                                            "trigger": (
                                                "Call first_item with an empty slice."
                                                if scenario == "valid"
                                                else "Evaluate stale_flag after its source changes."
                                            ),
                                            "failure": (
                                                "The direct index panics instead of returning absence."
                                                if scenario == "valid"
                                                else "The report cites bytes that are no longer current."
                                            ),
                                            "impact": (
                                                "A caller-controlled empty collection terminates the operation."
                                                if scenario == "valid"
                                                else "Users could be shown a finding against obsolete source."
                                            ),
                                            "category": "correctness",
                                            "verdict": "CONFIRMED",
                                            "outcome": "skipped",
                                        }
                                    ]
                                },
                            )
                        else:
                            latest_content = str(tool_results[-1].get("content", ""))
                            if scenario == "valid":
                                assert '"published":false' in latest_content.replace(" ", "")
                                chunk(
                                    {
                                        "content": (
                                            "REVIEWER_REPORT_SUBMITTED — source "
                                            f"review-target.rs:2 at revision {revisions['valid']}."
                                        )
                                    }
                                )
                            else:
                                assert STALE_ERROR in latest_content, latest_content
                                chunk(
                                    {
                                        "content": (
                                            "REVIEWER_STALE_REJECTED — source stale-target.rs:2 "
                                            f"at obsolete revision {revisions['stale']}."
                                        )
                                    }
                                )
                            chunk({}, "stop")
                elif PARENT_VALID in last_user:
                    if not tool_results:
                        call(
                            "agent",
                            {
                                "label": "Review indexing defect",
                                "prompt": (
                                    f"{CHILD_VALID}: read review-target.rs, report the concrete "
                                    "defect with report_findings using the exact read revision, "
                                    "then cite the source path, line, and revision in your answer."
                                ),
                                "agent": "reviewer",
                                "background": False,
                            },
                        )
                    else:
                        chunk({"content": "PARENT_VALID_COMPLETE"})
                        chunk({}, "stop")
                elif PARENT_STALE in last_user:
                    if not tool_results:
                        call(
                            "agent",
                            {
                                "label": "Review stale source",
                                "prompt": (
                                    f"{CHILD_STALE}: read stale-target.rs, then report its "
                                    "finding with the exact read revision and cite that source."
                                ),
                                "agent": "reviewer",
                                "background": False,
                            },
                        )
                    else:
                        chunk({"content": "PARENT_STALE_COMPLETE"})
                        chunk({}, "stop")
                else:
                    chunk({"content": "REPORT_FINDINGS_FIXTURE_READY"})
                    chunk({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    os.environ["HEYCODE_REPORT_FINDINGS_FIXTURE"] = "local-only-not-a-real-credential"
    captures: list[str] = []
    assertions: list[str] = []
    live_transcript = b""
    replay_transcript = b""

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-report-findings-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            source_root["workspace"] = workspace
            (workspace / "review-target.rs").write_text(
                "pub fn first_item(items: &[u8]) -> u8 {\n    items[0]\n}\n"
            )
            (workspace / "stale-target.rs").write_text(
                "pub fn stale_flag() -> bool {\n    false\n}\n"
            )
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "settings.toml").write_text(
                f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n'
            )
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_REPORT_FINDINGS_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            extra = [
                "--provider",
                "openrouter",
                "--model",
                MODEL["id"],
                "--approval",
                "default",
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_REPORT_FINDINGS_FIXTURE",
            ]

            def launch(
                resume: Path | None = None,
            ) -> tuple[FullScreenTui, Screen, TerminalByteStream]:
                tui = FullScreenTui(
                    str(home),
                    str(workspace),
                    str(binary),
                    fake=False,
                    rows=rows,
                    columns=columns,
                    color=color,
                    extra=extra + (["--resume", str(resume)] if resume else []),
                )
                screen = Screen(columns, rows)
                return tui, screen, TerminalByteStream(screen)

            tui, screen, stream = launch()

            def read(seconds: float = 0.12) -> str:
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def send(data: bytes) -> None:
                os.write(tui.fd, data)
                read(0.12)

            def visible() -> str:
                return "\n".join(screen.display)

            def wait_for(needle: str, timeout: float = 45) -> str:
                deadline = time.monotonic() + timeout
                current = ""
                while time.monotonic() < deadline:
                    current = read()
                    if needle in current:
                        return current
                    if not tui.alive():
                        raise AssertionError(
                            f"CLI exited while waiting for {needle!r}:\n{current}"
                        )
                raise AssertionError(f"Missing {needle!r}:\n{current}")

            def capture(name: str) -> str:
                current = read(0.3)
                (output / f"{name}.txt").write_text(current)
                render_screen(
                    screen,
                    output / f"{name}.png",
                    background="#ffffff" if theme == "heycode-light" else "#101014",
                    foreground="#24292f" if theme == "heycode-light" else "#dddddd",
                )
                captures.append(name)
                return current

            def capture_narrow(name: str) -> None:
                screen.resize(24, 60)
                fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 60, 0, 0))
                read(0.3)
                capture(name)
                screen.resize(rows, columns)
                fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
                read(0.3)

            def click_text(pattern: str) -> None:
                read(0.2)
                for row, line in enumerate(screen.display):
                    match = re.search(pattern, line)
                    if match:
                        column = match.start() + 1
                        terminal_row = row + 1
                        send(f"\x1b[<0;{column};{terminal_row}M".encode())
                        send(f"\x1b[<0;{column};{terminal_row}m".encode())
                        return
                raise AssertionError(
                    f"No clickable text matching {pattern!r}:\n{visible()}"
                )

            def approve_pending(expected_tool: str, capture_name: str) -> None:
                pending = wait_for("Esc to cancel")
                deadline = time.monotonic() + 20
                while expected_tool.lower() not in pending.lower() and time.monotonic() < deadline:
                    pending = read()
                assert expected_tool.lower() in pending.lower(), pending
                capture(capture_name)
                send(b"y")

            def stop_current() -> bytes:
                nonlocal tui
                send(b"/quit\r")
                deadline = time.monotonic() + 8
                while tui.alive() and time.monotonic() < deadline:
                    read(0.1)
                transcript = tui.transcript
                tui.close()
                return transcript

            try:
                initial = read(2)
                if "Welcome to heycode" in initial:
                    send(b"\x1b[B\x1b[B\r")
                    wait_for("Select a provider")
                    send(b"OpenRouter\r")
                    wait_for("Paste your OpenRouter API key")
                    send(b"local-only-not-a-real-credential\r")
                    wait_for("Choose a model")
                    send(b"\r")
                wait_for("? for shortcuts")
                capture("00-ready")

                send(f"{PARENT_VALID}\r".encode())
                approve_pending("agent", "01-reviewer-task-pending")
                approve_pending("read", "02-review-source-read-pending")
                approve_pending("report_findings", "03-report-findings-pending")
                wait_for("PARENT_VALID_COMPLETE", 60)
                completed = wait_for("Code review(high · 1 finding)")
                assert "⎿  review-target.rs" in completed, completed
                assert "● 2 [correctness] Unchecked indexing can panic" in completed, completed
                capture("04-report-completed-collapsed")
                capture_narrow("04-report-completed-collapsed-narrow")
                click_text(r"Code review\(high · 1 finding\)")
                expanded = wait_for("Local report; not externally published.")
                for text in [
                    "severity: high",
                    "trigger: Call first_item with an empty slice.",
                    "failure: The direct index panics instead of returning absence.",
                    "impact: A caller-controlled empty collection terminates",
                    "verdict: CONFIRMED",
                    "outcome: skipped",
                    "revision:",
                ]:
                    assert text in expanded, (text, expanded)
                capture("05-report-completed-expanded")
                capture_narrow("05-report-completed-expanded-narrow")
                assertions.append(
                    "annotated report rendered review level and category when collapsed plus severity, verdict, outcome, causal evidence, and revision when expanded"
                )

                send(b"\x1b")
                send(f"{PARENT_STALE}\r".encode())
                approve_pending("agent", "06-stale-reviewer-task-pending")
                approve_pending("read", "07-stale-source-read-pending")
                approve_pending("report_findings", "08-stale-report-pending")
                refused = wait_for("PARENT_STALE_COMPLETE", 60)
                assert "REVIEWER_STALE_REJECTED" in refused, refused
                capture("09-stale-source-refused")
                assertions.append(
                    "a file changed after read was rejected without a second report card"
                )

                logs = sorted(home.rglob("session.jsonl"))
                assert len(logs) == 3, logs
                classified: dict[str, tuple[Path, list[dict[str, Any]]]] = {}
                for path in logs:
                    events = [
                        json.loads(line)
                        for line in path.read_text().splitlines()
                        if line.strip()
                    ]
                    text = json.dumps(events, ensure_ascii=False)
                    if PARENT_VALID in text:
                        classified["parent"] = (path, events)
                    elif CHILD_VALID in text:
                        classified["valid_child"] = (path, events)
                    elif CHILD_STALE in text:
                        classified["stale_child"] = (path, events)
                assert set(classified) == {"parent", "valid_child", "stale_child"}, classified

                parent_path, parent_events = classified["parent"]
                reports = [
                    event["data"]["change"]["report"]
                    for event in parent_events
                    if event.get("kind") == "review/change"
                    and event.get("data", {})
                    .get("change", {})
                    .get("operation")
                    == "findings_reported"
                ]
                assert len(reports) == 1, reports
                report = reports[0]
                finding = report["findings"][0]
                assert report["level"] == "high", report
                assert finding["path"] == "review-target.rs", finding
                assert finding["line_start"] == finding["line_end"] == 2, finding
                assert finding["revision"] == revisions["valid"], (finding, revisions)
                assert finding["category"] == "correctness", finding
                assert finding["verdict"] == "CONFIRMED", finding
                assert finding["outcome"] == "skipped", finding
                assert report["source"]["session_id"] == parent_path.parent.name, report
                assert report["source"]["cwd"] is None, report
                assert stale_mutated.is_set()
                assert revisions["valid"] != revisions["stale"]

                _, stale_events = classified["stale_child"]
                stale_results = [
                    event
                    for event in stale_events
                    if event.get("kind") == "tool/result"
                    and event.get("data", {}).get("is_error") is True
                ]
                assert any(
                    STALE_ERROR in str(event.get("data", {}).get("content", ""))
                    for event in stale_results
                ), stale_results
                for name, (path, events) in classified.items():
                    shutil.copyfile(path, output / f"{name}-session.jsonl")
                    (output / f"{name}-events.json").write_text(
                        json.dumps(events, indent=2, ensure_ascii=False)
                    )
                assertions.append(
                    "parent JSONL contains exactly one authority-derived findings_reported event"
                )

                requests_before_resume = len(requests)
                live_transcript = stop_current()
                (output / "live.ansi").write_bytes(live_transcript)

                # Recreate the exact missing-field shape written before the
                # optional reference dimensions existed, while retaining the
                # live authority-derived source and all required evidence.
                legacy_events = json.loads(json.dumps(parent_events))
                legacy_reports = [
                    event["data"]["change"]["report"]
                    for event in legacy_events
                    if event.get("kind") == "review/change"
                    and event.get("data", {})
                    .get("change", {})
                    .get("operation")
                    == "findings_reported"
                ]
                assert len(legacy_reports) == 1, legacy_reports
                legacy_report = legacy_reports[0]
                legacy_report.pop("level", None)
                for legacy_finding in legacy_report["findings"]:
                    legacy_finding.pop("category", None)
                    legacy_finding.pop("verdict", None)
                    legacy_finding.pop("outcome", None)
                parent_path.write_text(
                    "".join(
                        json.dumps(event, separators=(",", ":"), ensure_ascii=False)
                        + "\n"
                        for event in legacy_events
                    )
                )
                shutil.copyfile(parent_path, output / "legacy-input-session.jsonl")
                (output / "legacy-input-events.json").write_text(
                    json.dumps(legacy_events, indent=2, ensure_ascii=False)
                )

                tui, screen, stream = launch(parent_path)
                wait_for("Code review(1 finding)")
                replayed = capture("10-legacy-reopened-collapsed")
                assert "⎿  review-target.rs" in replayed, replayed
                assert "● 2 Unchecked indexing can panic" in replayed, replayed
                assert "Code review(high · 1 finding)" not in replayed, replayed
                assert "[correctness]" not in replayed, replayed
                assert "REVIEWER_STALE_REJECTED" in replayed, replayed
                assert len(requests) == requests_before_resume, (
                    requests_before_resume,
                    len(requests),
                )
                click_text(r"Code review\(1 finding\)")
                replayed_expanded = wait_for("Local report; not externally published.")
                assert revisions["valid"][:12] in replayed_expanded, replayed_expanded
                assert "severity: high" in replayed_expanded, replayed_expanded
                assert "verdict:" not in replayed_expanded, replayed_expanded
                assert "outcome:" not in replayed_expanded, replayed_expanded
                capture("11-legacy-reopened-expanded")
                time.sleep(0.5)
                read(0.2)
                assert len(requests) == requests_before_resume
                assertions.append(
                    "old unannotated journal replayed with grouped fallback UI and no provider request"
                )
                replay_transcript = stop_current()
                (output / "replay.ansi").write_bytes(replay_transcript)

                current_parent_events = [
                    json.loads(line)
                    for line in parent_path.read_text().splitlines()
                    if line.strip()
                ]
                current_reports = [
                    event
                    for event in current_parent_events
                    if event.get("kind") == "review/change"
                    and event.get("data", {})
                    .get("change", {})
                    .get("operation")
                    == "findings_reported"
                ]
                assert len(current_reports) == 1, current_reports
                shutil.copyfile(parent_path, output / "legacy-after-reopen-session.jsonl")
                (output / "legacy-after-reopen-events.json").write_text(
                    json.dumps(current_parent_events, indent=2, ensure_ascii=False)
                )

                assert set(reviewer_catalogs) == {"valid", "stale"}
                assert set(reviewer_schemas) == {"valid", "stale"}
                assert all(
                    "report_findings" in catalog for catalog in reviewer_catalogs.values()
                )
                assert all("write" not in catalog for catalog in reviewer_catalogs.values())
                assert all("bash" not in catalog for catalog in reviewer_catalogs.values())
                assertions.append(
                    "reviewer catalogs contain report_findings but exclude write and bash"
                )
                assertions.append(
                    "advertised report schema caps new calls at 32 and exposes the exact optional reference dimensions"
                )

                result: dict[str, object] = {
                    "status": "passed",
                    "scope": "production heycode CLI/TUI/native reviewer with localhost SSE only",
                    "external_provider": False,
                    "binary": str(binary),
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "provider_requests": len(requests),
                    "provider_requests_on_reopen": len(requests) - requests_before_resume,
                    "parent_session_id": parent_path.parent.name,
                    "accepted_report_count": 1,
                    "accepted_review_level": report["level"],
                    "accepted_category": finding["category"],
                    "accepted_verdict": finding["verdict"],
                    "accepted_outcome": finding["outcome"],
                    "accepted_revision": revisions["valid"],
                    "stale_revision": revisions["stale"],
                    "stale_revision_refused": True,
                    "legacy_unannotated_replay": True,
                    "reviewer_catalogs": reviewer_catalogs,
                    "report_findings_max_items": reviewer_schemas["valid"][
                        "properties"
                    ]["findings"]["maxItems"],
                    "captures": captures,
                    "assertions": assertions,
                }
                (output / "result.json").write_text(
                    json.dumps(result, indent=2, ensure_ascii=False)
                )
                print(json.dumps(result, indent=2, ensure_ascii=False))
                return result
            except Exception:
                try:
                    capture("failure")
                finally:
                    (output / "failure.ansi").write_bytes(tui.transcript)
                raise
            finally:
                if tui.alive():
                    tui.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)
        (output / "requests.json").write_text(
            json.dumps(requests, indent=2, ensure_ascii=False)
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("tmp/cli-snapshots/d9a9c1f29cafc51e/dshx-20260911T100713Z-40e433"),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5"),
    )
    parser.add_argument(
        "--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark"
    )
    parser.add_argument("--color", action="store_true")
    parser.add_argument("--columns", type=int, default=120)
    parser.add_argument("--rows", type=int, default=52)
    arguments = parser.parse_args()
    run(
        arguments.binary,
        arguments.output,
        theme=arguments.theme,
        color=arguments.color,
        columns=arguments.columns,
        rows=arguments.rows,
    )
