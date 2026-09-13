#!/usr/bin/env python3
"""Capture heycode `/compact` working, completion, failure, and Escape states.

The production CLI talks only to a deterministic OpenRouter-shaped HTTP/SSE
fixture bound to 127.0.0.1. Every scenario receives a disposable HEYCODE_HOME and
workspace. Three synthetic turns create real durable history before one
focused manual compaction request is held, completed, failed, or interrupted.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import socket
import threading
import time
from tempfile import TemporaryDirectory
from typing import Any

import pyte

from agent_navigation_terminal_check import MODEL, Screen
from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


FOCUS = "prioritize API decisions, cancellation, and exact filenames"
COMMAND = f"/compact {FOCUS}"
CURSOR_LEFT = 10
CURSOR_PROBE = "X"
SYNTHETIC_PROMPTS = [
    "SYNTHETIC-ALPHA transport abstraction",
    "SYNTHETIC-BETA crates/example/src/lib.rs",
    "SYNTHETIC-GAMMA cancellation leaves the journal unchanged",
]


def run(
    binary: Path,
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    lock = threading.Lock()
    requests: list[dict[str, object]] = []
    fixture = {
        "scenario": "idle",
        "phase": "idle",
        "mode": "complete",
        "scenario_output": None,
    }
    entered = threading.Event()
    release = threading.Event()
    settled = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def json_response(self, status: int, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            if self.path.endswith("/key"):
                value: object = {"data": {"label": "local compact fixture"}}
            elif "/model/" in self.path:
                value = {"data": MODEL}
            else:
                value = {
                    "data": [MODEL],
                    "total_count": 1,
                    "links": {"next": None},
                }
            self.json_response(200, value)

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            with lock:
                scenario = str(fixture["scenario"])
                phase = str(fixture["phase"])
                mode = str(fixture["mode"])
                scenario_output = fixture["scenario_output"]
            wire = raw.decode(errors="replace")
            record: dict[str, object] = {
                "path": self.path,
                "scenario": scenario,
                "phase": phase,
                "mode": mode,
                "loopback": True,
                "request_bytes": len(raw),
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "tool_count": len(body.get("tools", [])),
                "focus_present": FOCUS in wire,
                "synthetic_markers_present": [
                    marker for marker in SYNTHETIC_PROMPTS if marker in wire
                ],
                "client_disconnected": False,
            }
            with lock:
                requests.append(record)
            if phase == "compact":
                assert isinstance(scenario_output, Path)
                (scenario_output / "compact-request.json").write_text(
                    json.dumps(body, indent=2)
                )
                entered.set()

            try:
                if phase == "compact" and mode == "failure":
                    if not release.wait(timeout):
                        raise TimeoutError("failure fixture was not released")
                    self.json_response(
                        400,
                        {
                            "error": {
                                "message": "LOCAL_HEYCODE_COMPACT_FAILURE",
                                "type": "invalid_request_error",
                                "code": 400,
                            }
                        },
                    )
                    return

                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if phase == "compact" and not release.wait(timeout):
                    raise TimeoutError("compact fixture was not released")

                def chunk(text: str | None, finish: str | None = None) -> None:
                    delta: dict[str, object] = {}
                    if text is not None:
                        delta["content"] = text
                    payload = {
                        "id": f"heycode-compact-{scenario}-{phase}",
                        "object": "chat.completion.chunk",
                        "model": MODEL["id"],
                        "choices": [
                            {
                                "index": 0,
                                "delta": delta,
                                "finish_reason": finish,
                            }
                        ],
                    }
                    self.wfile.write(
                        ("data: " + json.dumps(payload) + "\n\n").encode()
                    )
                    self.wfile.flush()

                if phase == "seed":
                    chunk(f"LOCAL-SEED-REPLY-{scenario}")
                elif scenario == "cancel":
                    # A small write can still fit in the local kernel send
                    # buffer after the response consumer has gone away. Fill
                    # that buffer under a short timeout so this fixture records
                    # transport abandonment rather than inferring it from UI.
                    self.connection.settimeout(0.5)
                    for _ in range(256):
                        chunk("LOCAL-CANCEL-DISCONNECT-PROBE-" + ("x" * 16_384))
                else:
                    chunk(
                        "LOCAL HEYCODE COMPACT SUMMARY: preserve the API decision, "
                        "cancellation rule, and crates/example/src/lib.rs."
                    )
                chunk(None, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, socket.timeout):
                record["client_disconnected"] = True
            finally:
                if phase == "compact":
                    settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    result: dict[str, object] = {
        "status": "failed",
        "scope": "heycode compact UI with localhost provider and synthetic history",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
        "normal_conversation_prompts": 0,
        "credential_source": "dummy loopback key only",
        "scenarios": {},
    }

    try:
        for scenario in ("complete", "failure", "cancel"):
            scenario_output = output / scenario
            scenario_output.mkdir()
            entered.clear()
            release.clear()
            settled.clear()
            with lock:
                fixture.update(
                    {
                        "scenario": scenario,
                        "phase": "seed",
                        "mode": "complete",
                        "scenario_output": scenario_output,
                    }
                )

            with TemporaryDirectory(prefix=f"heycode-compact-{scenario}-") as temporary:
                root = Path(temporary)
                home = root / "home"
                workspace = root / "workspace"
                home.mkdir()
                workspace.mkdir()
                base = f"http://127.0.0.1:{server.server_port}/api/v1"
                (home / "settings.toml").write_text("schema_version = 1\n")
                (home / "config.toml").write_text(
                    "schema_version = 31\n"
                    "[llm]\n"
                    'provider = "openrouter"\n'
                    f'model = "{MODEL["id"]}"\n'
                    'api_key_env = "HEYCODE_COMPACT_STATES_FIXTURE"\n'
                    f'base_url = "{base}"\n'
                )
                os.environ["HEYCODE_COMPACT_STATES_FIXTURE"] = "local-only-dummy"
                tui = FullScreenTui(
                    str(home),
                    str(workspace),
                    str(binary),
                    fake=False,
                    color=True,
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
                        "llm.api_key_env=HEYCODE_COMPACT_STATES_FIXTURE",
                    ],
                )
                screen: pyte.Screen = Screen(columns, rows)
                stream = pyte.ByteStream(screen)
                captures: list[str] = []

                def read(seconds: float = 0.15) -> str:
                    stream.feed(tui.read(seconds))
                    return visible()

                def visible() -> str:
                    return "\n".join(screen.display)

                def wait_for_text(*needles: str, wait_timeout: float = 30) -> str:
                    deadline = time.monotonic() + wait_timeout
                    latest = ""
                    while time.monotonic() < deadline:
                        latest = read()
                        if any(needle.lower() in latest.lower() for needle in needles):
                            return latest
                        if not tui.alive():
                            raise RuntimeError(
                                f"heycode exited while waiting for {needles}:\n{latest}"
                            )
                    raise TimeoutError(f"timed out waiting for {needles}:\n{latest}")

                def wait_event(event: threading.Event, label: str) -> None:
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        read()
                        if event.is_set():
                            return
                        if not tui.alive():
                            raise RuntimeError(f"heycode exited while waiting for {label}")
                    raise TimeoutError(f"timed out waiting for {label}:\n{visible()}")

                def send(data: bytes, seconds: float = 0.15) -> str:
                    os.write(tui.fd, data)
                    return read(seconds)

                def capture(name: str) -> str:
                    text = read(0.35)
                    (scenario_output / f"{name}.txt").write_text(text)
                    render_screen(screen, scenario_output / f"{name}.png")
                    captures.append(name)
                    return text

                def click_text(text: str) -> str:
                    for row, line in enumerate(screen.display):
                        if text in line:
                            column = line.index(text)
                            sequence = (
                                f"\x1b[<0;{column + 1};{row + 1}M"
                                f"\x1b[<0;{column + 1};{row + 1}m"
                            )
                            return send(sequence.encode(), 0.4)
                    raise AssertionError(f"mouse target {text!r} is not visible")

                def log_path() -> Path:
                    logs = list(home.rglob("session.jsonl"))
                    if len(logs) != 1:
                        raise AssertionError(logs)
                    return logs[0]

                def events() -> list[dict[str, Any]]:
                    return [
                        json.loads(line)
                        for line in log_path().read_text().splitlines()
                        if line
                    ]

                def count(kind: str) -> int:
                    return sum(event["kind"] == kind for event in events())

                try:
                    wait_for_text("shift+tab to cycle", wait_timeout=40)
                    for index, prompt in enumerate(SYNTHETIC_PROMPTS, 1):
                        send(prompt.encode() + b"\r")
                        wait_for_text(f"LOCAL-SEED-REPLY-{scenario}")
                        deadline = time.monotonic() + timeout
                        while count("turn/end") < index and time.monotonic() < deadline:
                            read()
                        assert count("turn/end") == index
                    capture("00-ready-with-synthetic-history")
                    before = log_path().read_bytes()

                    with lock:
                        fixture["phase"] = "compact"
                        fixture["mode"] = "failure" if scenario == "failure" else "complete"
                    if scenario == "cancel":
                        send(COMMAND.encode())
                        send(b"\x1b[D" * CURSOR_LEFT)
                        expected_cursor_offset = len(COMMAND) - CURSOR_LEFT
                        send(b"\r")
                    else:
                        expected_cursor_offset = None
                        send(COMMAND.encode() + b"\r")
                    wait_event(entered, "held compact request")
                    held = capture("01-held")

                    after_escape = None
                    cursor_probe = None
                    if scenario == "cancel":
                        send(b"\x1b", 0.8)
                        after_escape = capture("02-after-escape")
                        assert expected_cursor_offset is not None
                        expected_probe = (
                            COMMAND[:expected_cursor_offset]
                            + CURSOR_PROBE
                            + COMMAND[expected_cursor_offset:]
                        )
                        send(CURSOR_PROBE.encode(), 0.3)
                        cursor_probe = capture("03-cursor-probe")
                        send(b"\x7f", 0.3)
                    release.set()
                    wait_event(settled, "compact fixture settlement")
                    if scenario == "complete":
                        wait_for_text("Compacted", "folded")
                        final_ui = capture("02-completed")
                        send(b"\x0f", 0.5)
                        expanded_ui = capture("03-expanded")
                        send(b"\x0f", 0.5)
                        keyboard_collapsed_ui = capture("04-keyboard-collapsed")
                        click_text("Compacted")
                        mouse_expanded_ui = capture("05-mouse-expanded")
                        click_text("Compacted")
                        mouse_collapsed_ui = capture("06-mouse-collapsed")
                    elif scenario == "failure":
                        wait_for_text("LOCAL_HEYCODE_COMPACT_FAILURE", "compaction failed")
                        final_ui = capture("02-failed")
                    else:
                        read(1)
                        final_ui = capture("04-after-provider-release")

                    final_bytes = log_path().read_bytes()
                    final_events = events()
                    boundaries = [
                        event
                        for event in final_events
                        if event["kind"] == "compaction/applied"
                    ]
                    scenario_requests = [
                        request
                        for request in requests
                        if request["scenario"] == scenario
                        and request["phase"] == "compact"
                    ]
                    compact_contract = {
                        "request_entered": bool(scenario_requests),
                        "focus_present": bool(scenario_requests)
                        and all(request["focus_present"] for request in scenario_requests),
                        "folded_synthetic_marker_present": bool(scenario_requests)
                        and all(
                            SYNTHETIC_PROMPTS[0]
                            in request["synthetic_markers_present"]
                            for request in scenario_requests
                        ),
                        "held_working_label": "compact" in held.lower()
                        and ("interrupt" in held.lower() or "esc" in held.lower()),
                        "seeded_prefix_byte_exact": final_bytes.startswith(before),
                    }
                    if scenario == "complete":
                        compact_contract.update(
                            {
                                "one_compaction_boundary": len(boundaries) == 1,
                                "completed_receipt": "folded" in final_ui.lower(),
                                "summary_collapsed_by_default": (
                                    "LOCAL HEYCODE COMPACT SUMMARY" not in final_ui
                                ),
                                "summary_expandable": "ctrl+o" in final_ui.lower()
                                or "expand" in final_ui.lower(),
                                "expanded_summary_visible": (
                                    "LOCAL HEYCODE COMPACT SUMMARY" in expanded_ui
                                ),
                                "keyboard_collapse": (
                                    "LOCAL HEYCODE COMPACT SUMMARY"
                                    not in keyboard_collapsed_ui
                                ),
                                "mouse_expand": (
                                    "LOCAL HEYCODE COMPACT SUMMARY" in mouse_expanded_ui
                                ),
                                "mouse_collapse": (
                                    "LOCAL HEYCODE COMPACT SUMMARY"
                                    not in mouse_collapsed_ui
                                ),
                            }
                        )
                    elif scenario == "failure":
                        compact_contract.update(
                            {
                                "no_compaction_boundary": not boundaries,
                                "explicit_failure_receipt": (
                                    "compaction failed: provider rejected the request"
                                    in final_ui.lower()
                                ),
                                "journal_byte_exact": final_bytes == before,
                            }
                        )
                    else:
                        assert (
                            after_escape is not None
                            and cursor_probe is not None
                            and expected_cursor_offset is not None
                        )
                        command_restored = COMMAND in after_escape
                        compact_contract.update(
                            {
                                "no_compaction_boundary": not boundaries,
                                "request_disconnected": bool(scenario_requests)
                                and any(
                                    request["client_disconnected"]
                                    for request in scenario_requests
                                ),
                                "full_command_restored": command_restored,
                                "cursor_restored": expected_probe in cursor_probe,
                                "journal_byte_exact": final_bytes == before,
                            }
                        )

                    (scenario_output / "final-session.jsonl").write_bytes(final_bytes)
                    (scenario_output / "events.json").write_text(
                        json.dumps(final_events, indent=2)
                    )
                    result["scenarios"][scenario] = {
                        "captures": captures,
                        "message_requests": len(scenario_requests),
                        "compact_boundaries": len(boundaries),
                        "expected_cursor_offset": expected_cursor_offset,
                        "cursor_probe": (
                            expected_probe if scenario == "cancel" else None
                        ),
                        "contract": compact_contract,
                    }
                finally:
                    release.set()
                    (scenario_output / "terminal.ansi").write_bytes(tui.transcript)
                    tui.close()

        scenario_contracts = [
            value["contract"] for value in result["scenarios"].values()
        ]
        failures = [
            f"{scenario}.{check}"
            for scenario, value in result["scenarios"].items()
            for check, passed in value["contract"].items()
            if not passed
        ]
        result.update(
            {
                "status": "passed" if not failures else "gaps_observed",
                "contract_failures": failures,
                "localhost_message_requests": len(requests),
                "synthetic_conversation_prompts": len(SYNTHETIC_PROMPTS) * 3,
                "all_recorded_requests_loopback": all(
                    request["loopback"] for request in requests
                ),
                "all_scenario_contracts_recorded": len(scenario_contracts) == 3,
            }
        )
    except Exception as error:  # preserve partial artifacts for diagnosis
        result.update(
            {"status": "blocked", "error": f"{type(error).__name__}: {error}"}
        )
    finally:
        release.set()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        (output / "requests-summary.json").write_text(json.dumps(requests, indent=2))
        (output / "result.json").write_text(json.dumps(result, indent=2))

    print(json.dumps(result, indent=2))
    return result


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=50)
    parser.add_argument("--timeout", type=float, default=30)
    arguments = parser.parse_args()
    outcome = run(
        arguments.binary,
        arguments.output,
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
    raise SystemExit(0 if outcome["status"] == "passed" else 1)
