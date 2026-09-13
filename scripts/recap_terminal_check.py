#!/usr/bin/env python3
"""Capture native heycode `/recap` states against a localhost provider.

Each scenario uses a disposable HEYCODE_HOME and workspace. Three real synthetic
turns establish durable history; the recap side request is then held,
completed, rejected, or given an Escape key. All provider traffic is forced to
an OpenRouter-shaped fixture bound to 127.0.0.1 with a dummy key.
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


SYNTHETIC_PROMPTS = [
    "SYNTHETIC-RECAP-ALPHA finish the local command audit",
    "SYNTHETIC-RECAP-BETA preserve the no-paid-provider boundary",
    "SYNTHETIC-RECAP-GAMMA next compare native terminal evidence",
]
RECAP_TEXT = (
    "LOCAL HEYCODE RECAP: finish the local command audit, preserve the no-paid-"
    "provider boundary, then compare native terminal evidence."
)


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
    fixture: dict[str, object] = {
        "scenario": "idle",
        "phase": "idle",
        "mode": "complete",
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
                value: object = {"data": {"label": "local recap fixture"}}
            elif "/model/" in self.path:
                value = {"data": MODEL}
            else:
                value = {"data": [MODEL], "total_count": 1, "links": {"next": None}}
            self.json_response(200, value)

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            with lock:
                scenario = str(fixture["scenario"])
                phase = str(fixture["phase"])
                mode = str(fixture["mode"])
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
                "recap_instruction_present": "Summarize the current task" in wire,
                "synthetic_markers_present": [
                    marker for marker in SYNTHETIC_PROMPTS if marker in wire
                ],
                "client_disconnected": False,
            }
            with lock:
                requests.append(record)
            if phase == "recap":
                entered.set()

            try:
                if phase == "recap" and mode == "failure":
                    if not release.wait(timeout):
                        raise TimeoutError("failure fixture was not released")
                    self.json_response(
                        400,
                        {
                            "error": {
                                "message": "LOCAL_HEYCODE_RECAP_FAILURE",
                                "type": "invalid_request_error",
                                "code": 400,
                            }
                        },
                    )
                    return

                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if phase == "recap" and not release.wait(timeout):
                    raise TimeoutError("recap fixture was not released")

                def chunk(text: str | None, finish: str | None = None) -> None:
                    delta: dict[str, object] = {}
                    if text is not None:
                        delta["content"] = text
                    payload = {
                        "id": f"heycode-recap-{scenario}-{phase}",
                        "object": "chat.completion.chunk",
                        "model": MODEL["id"],
                        "choices": [
                            {"index": 0, "delta": delta, "finish_reason": finish}
                        ],
                    }
                    self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                    self.wfile.flush()

                if phase == "seed":
                    chunk(f"LOCAL-SEED-REPLY-{scenario}")
                elif scenario == "cancel":
                    self.connection.settimeout(0.5)
                    chunk(RECAP_TEXT)
                    for _ in range(256):
                        chunk("LOCAL-CANCEL-DISCONNECT-PROBE-" + ("x" * 16_384))
                else:
                    chunk(RECAP_TEXT)
                chunk(None, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, socket.timeout):
                record["client_disconnected"] = True
            finally:
                if phase == "recap":
                    settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    result: dict[str, object] = {
        "status": "failed",
        "scope": "native heycode recap UI with localhost provider and real synthetic turns",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
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
                fixture.update({"scenario": scenario, "phase": "seed", "mode": "complete"})

            with TemporaryDirectory(prefix=f"heycode-recap-{scenario}-") as temporary:
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
                    'api_key_env = "HEYCODE_RECAP_FIXTURE"\n'
                    f'base_url = "{base}"\n'
                )
                os.environ["HEYCODE_RECAP_FIXTURE"] = "local-only-dummy"
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
                        "llm.api_key_env=HEYCODE_RECAP_FIXTURE",
                    ],
                )
                screen: pyte.Screen = Screen(columns, rows)
                stream = pyte.ByteStream(screen)
                captures: list[str] = []

                def visible() -> str:
                    return "\n".join(screen.display)

                def read(seconds: float = 0.15) -> str:
                    stream.feed(tui.read(seconds))
                    return visible()

                def wait_for_text(*needles: str, wait_timeout: float = 30) -> str:
                    deadline = time.monotonic() + wait_timeout
                    latest = visible()
                    while time.monotonic() < deadline:
                        latest = read()
                        normalized = "".join(latest.split()).lower()
                        if any("".join(needle.split()).lower() in normalized for needle in needles):
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

                    if scenario == "complete":
                        send(b"/recap off\r")
                        wait_for_text("automatic return recap off")
                        send(b"/recap on\r")
                        wait_for_text("automatic return recap on")
                        send(b"/recap unexpected\r")
                        wait_for_text("usage: /recap [on|off]")
                        capture("01-preferences-and-invalid")

                    with lock:
                        fixture["phase"] = "recap"
                        fixture["mode"] = "failure" if scenario == "failure" else "complete"
                    send(b"/recap\r")
                    wait_event(entered, "held recap request")
                    held = capture("02-held" if scenario == "complete" else "01-held")

                    if scenario == "cancel":
                        send(b"\x1b", 0.8)
                        after_escape = capture("02-after-escape")
                    else:
                        after_escape = None
                    release.set()
                    wait_event(settled, "recap fixture settlement")
                    if scenario == "complete":
                        final_ui = wait_for_text("Recap: LOCAL HEYCODE RECAP")
                        final_ui = capture("03-completed")
                    elif scenario == "failure":
                        final_ui = wait_for_text("LOCAL_HEYCODE_RECAP_FAILURE", "provider rejected")
                        final_ui = capture("02-failed")
                    else:
                        read(1.0)
                        final_ui = capture("03-after-provider-release")

                    final_bytes = log_path().read_bytes()
                    scenario_requests = [
                        request
                        for request in requests
                        if request["scenario"] == scenario and request["phase"] == "recap"
                    ]
                    contract = {
                        "request_entered": bool(scenario_requests),
                        "recap_instruction_present": bool(scenario_requests)
                        and all(request["recap_instruction_present"] for request in scenario_requests),
                        "all_synthetic_markers_present": bool(scenario_requests)
                        and all(
                            set(request["synthetic_markers_present"])
                            == set(SYNTHETIC_PROMPTS)
                            for request in scenario_requests
                        ),
                        "main_journal_byte_exact": final_bytes == before,
                        "held_working_state": "recap" in held.lower()
                        and ("interrupt" in held.lower() or "cancel" in held.lower()),
                    }
                    if scenario == "complete":
                        state_path = log_path().parent / "session-controls.json"
                        feature_state = json.loads(state_path.read_text())
                        contract.update(
                            {
                                "completed_receipt": "Recap: LOCAL HEYCODE RECAP" in final_ui,
                                "preference_round_trip": feature_state["auto_recap"] is True,
                                "invalid_usage_receipt": True,
                            }
                        )
                    elif scenario == "failure":
                        contract["explicit_failure_receipt"] = (
                            "provider rejected" in final_ui.lower()
                            or "LOCAL_HEYCODE_RECAP_FAILURE" in final_ui
                        )
                    else:
                        assert after_escape is not None
                        contract.update(
                            {
                                "request_disconnected": any(
                                    request["client_disconnected"]
                                    for request in scenario_requests
                                ),
                                "escape_removed_working_state": (
                                    "esc to interrupt" not in after_escape.lower()
                                    and "recapping" not in after_escape.lower()
                                    and "esc to cancel" not in after_escape.lower()
                                ),
                                "command_restored_to_composer": "/recap" in after_escape,
                                "no_completed_recap_after_escape": "Recap: LOCAL HEYCODE RECAP"
                                not in final_ui,
                            }
                        )

                    (scenario_output / "final-session.jsonl").write_bytes(final_bytes)
                    (scenario_output / "events.json").write_text(json.dumps(events(), indent=2))
                    result["scenarios"][scenario] = {
                        "captures": captures,
                        "message_requests": len(scenario_requests),
                        "contract": contract,
                    }
                finally:
                    release.set()
                    (scenario_output / "terminal.ansi").write_bytes(tui.transcript)
                    tui.close()

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
                "all_recorded_requests_loopback": all(request["loopback"] for request in requests),
            }
        )
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
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
