#!/usr/bin/env python3
"""Validate the production persistent advisor lifecycle over localhost only.

The caller supplies an immutable heycode binary. The journey uses a disposable
HEYCODE_HOME/workspace and a deterministic OpenRouter-shaped server bound to
127.0.0.1; it never reads the operator's credentials or contacts a paid API.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import tomllib
from typing import Any
from urllib.parse import unquote

import pyte

from agent_navigation_terminal_check import MODEL, Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MAIN_MODEL = "local/main-model"
ADVISOR_MODEL = "local/advisor-model"
QUESTION = "ADVISOR_LIFECYCLE_QUESTION: choose the safer persistence boundary"
GUIDANCE = "LOCAL ADVISOR GUIDANCE: separate durable route choice from live turn state."
FINAL = "LOCAL PARENT FINAL: applied the advisor guidance in this same parent turn."
SELECT = f"/advisor native:openrouter {ADVISOR_MODEL}"


def model(model_id: str, name: str) -> dict[str, Any]:
    value = json.loads(json.dumps(MODEL))
    value.update({"id": model_id, "canonical_slug": model_id, "name": name})
    value["reasoning"] = {
        "mandatory": False,
        "default_enabled": False,
        "supported_efforts": ["low", "high"],
        "default_effort": "low",
    }
    return value


MODELS = [
    MODEL,
    model(MAIN_MODEL, "Local parent fixture"),
    model(ADVISOR_MODEL, "Local advisor fixture"),
]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class Driver:
    def __init__(
        self,
        *,
        binary: Path,
        home: Path,
        workspace: Path,
        base_url: str,
        output: Path,
        phase: str,
        columns: int,
        rows: int,
        timeout: float,
        model_id: str | None = MAIN_MODEL,
    ) -> None:
        self.output = output
        self.phase = phase
        self.timeout = timeout
        self.tui = FullScreenTui(
            str(home),
            str(workspace),
            str(binary),
            fake=False,
            color=True,
            rows=rows,
            columns=columns,
            extra=[
                *(["--provider", "openrouter", "--model", model_id] if model_id is not None else []),
                "--approval",
                "full_access",
                "--set",
                f"llm.base_url={base_url}",
                "--set",
                "llm.api_key_env=HEYCODE_ADVISOR_LIFECYCLE_FIXTURE",
            ],
        )
        self.screen: pyte.Screen = Screen(columns, rows)
        self.stream = TerminalByteStream(self.screen)

    def read(self, seconds: float = 0.15) -> str:
        self.stream.feed(self.tui.read(seconds))
        return "\n".join(self.screen.display)

    def wait(self, needle: str) -> str:
        deadline = time.monotonic() + self.timeout
        latest = self.read()
        while time.monotonic() < deadline:
            if needle.lower() in latest.lower():
                return latest
            if not self.tui.alive():
                raise RuntimeError(
                    f"heycode exited in {self.phase} while waiting for {needle!r}:\n{latest}"
                )
            latest = self.read()
        raise TimeoutError(
            f"timed out in {self.phase} waiting for {needle!r}:\n{latest}"
        )

    def command(self, command: str, expected: str) -> str:
        os.write(self.tui.fd, command.encode() + b"\r")
        return self.wait(expected)

    def escape(self) -> None:
        os.write(self.tui.fd, b"\x1b")
        self.read(0.25)

    def capture(self, name: str) -> str:
        visible = self.read(0.35)
        target = f"{self.phase}-{name}"
        (self.output / f"{target}.txt").write_text(visible)
        render_screen(self.screen, self.output / f"{target}.png")
        return visible

    def close(self) -> None:
        (self.output / f"{self.phase}-terminal.ansi").write_bytes(
            self.tui.transcript
        )
        self.tui.close()


def advisor_settings(path: Path) -> dict[str, Any]:
    document = tomllib.loads(path.read_text())
    return document["settings"]["advisor"]


def run(
    binary: Path,
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, Any]:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    requests: list[dict[str, Any]] = []
    catalog_requests: list[str] = []
    request_lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        def send_json(self, value: object, status: int = 200) -> None:
            data = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            catalog_requests.append(self.path)
            if self.path.endswith("/key"):
                self.send_json({"data": {"label": "local advisor lifecycle fixture"}})
                return
            if "/model/" in self.path:
                decoded_path = unquote(self.path)
                requested = decoded_path.split("/model/", 1)[1].split("?", 1)[0]
                selected = next(
                    (item for item in MODELS if item["id"] == requested),
                    MODEL
                    if requested == MODEL["id"]
                    else model(requested, "Local on-demand catalog fixture"),
                )
                self.send_json({"data": selected})
                return
            self.send_json(
                {"data": MODELS, "total_count": len(MODELS), "links": {"next": None}}
            )

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            messages = body.get("messages", [])
            encoded_messages = json.dumps(messages, sort_keys=True)
            tools = body.get("tools") or []
            record = {
                "index": 0,
                "method": "POST",
                "path": self.path,
                "loopback": self.client_address[0] == "127.0.0.1",
                "model": body.get("model"),
                "message_count": len(messages),
                "tool_count": len(tools),
                "tool_names": [
                    tool.get("function", {}).get("name")
                    for tool in tools
                    if isinstance(tool, dict)
                ],
                "question_present": QUESTION in encoded_messages,
                "guidance_present": GUIDANCE in encoded_messages,
                "request_bytes": len(raw),
                "request_sha256": hashlib.sha256(raw).hexdigest(),
            }
            with request_lock:
                record["index"] = len(requests) + 1
                requests.append(record)

            requested_model = body.get("model")
            if requested_model == MAIN_MODEL and QUESTION in encoded_messages and GUIDANCE not in encoded_messages:
                chunks = [
                    {
                        "delta": {
                            "reasoning": "Consult the configured advisor before deciding.",
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": "advisor-lifecycle-call",
                                    "type": "function",
                                    "function": {"name": "advisor", "arguments": "{}"},
                                }
                            ]
                        },
                        "finish_reason": None,
                    },
                    {"delta": {}, "finish_reason": "tool_calls"},
                ]
            elif requested_model == ADVISOR_MODEL:
                chunks = [
                    {"delta": {"content": GUIDANCE}, "finish_reason": None},
                    {"delta": {}, "finish_reason": "stop"},
                ]
            elif requested_model == MAIN_MODEL and GUIDANCE in encoded_messages:
                chunks = [
                    {"delta": {"content": FINAL}, "finish_reason": None},
                    {"delta": {}, "finish_reason": "stop"},
                ]
            else:
                self.send_json(
                    {"error": {"message": "unexpected localhost advisor request"}},
                    status=400,
                )
                return

            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for chunk in chunks:
                payload = {
                    "id": f"advisor-lifecycle-{record['index']}",
                    "object": "chat.completion.chunk",
                    "model": requested_model,
                    "choices": [{"index": 0, **chunk}],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    prior_key = os.environ.get("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE")
    os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = "local-only-dummy-key"
    result: dict[str, Any] = {
        "status": "blocked",
        "scope": "production persistent advisor lifecycle with localhost inference only",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
        "credential_source": "dummy loopback key only",
        "catalog_requests": catalog_requests,
        "provider_requests": requests,
        "terminal_size": {"columns": columns, "rows": rows},
    }

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-advisor-lifecycle-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            settings_path = home / "settings.toml"
            settings_path.write_text("schema_version = 1\n")
            base_url = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MAIN_MODEL}"\n'
                'api_key_env = "HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"\n'
                f'base_url = "{base_url}"\n'
            )

            def start(phase: str) -> Driver:
                driver = Driver(
                    binary=binary,
                    home=home,
                    workspace=workspace,
                    base_url=base_url,
                    output=output,
                    phase=phase,
                    columns=columns,
                    rows=rows,
                    timeout=timeout,
                )
                driver.wait("shift+tab to cycle")
                return driver

            initial = start("01-initial")
            try:
                initial.command("/advisor", "Enter to confirm")
                initial_disabled_screen = initial.capture("disabled")
                initial.escape()
                initial.command(SELECT, "Advisor set to")
                initial.capture("selected")
                if requests:
                    raise AssertionError("advisor configuration dispatched inference")
            finally:
                initial.close()
            selected_disk = advisor_settings(settings_path)

            restarted = start("02-restarted")
            try:
                restarted.command(
                    "/advisor status", f"enabled: native:openrouter · {ADVISOR_MODEL}"
                )
                selected_restart_screen = restarted.capture("selection-restored")
                restarted.command("/advisor off", "Advisor disabled")
                restarted.capture("disabled-committed")
                if requests:
                    raise AssertionError("advisor disable dispatched inference")
            finally:
                restarted.close()
            disabled_disk = advisor_settings(settings_path)

            lifecycle = start("03-lifecycle")
            try:
                lifecycle.command("/advisor status", "advisor disabled")
                disabled_restart_screen = lifecycle.capture("disabled-restored")
                lifecycle.command(SELECT, "Advisor set to")
                lifecycle.capture("reselected")
                lifecycle.command(QUESTION, FINAL)
                lifecycle.capture("same-turn-final")
            finally:
                lifecycle.close()

            logs = sorted(home.rglob("session.jsonl"))
            parent_matches: list[tuple[Path, list[dict[str, Any]]]] = []
            for path in logs:
                events = [
                    json.loads(line)
                    for line in path.read_text().splitlines()
                    if line.strip()
                ]
                if any(
                    event.get("kind") == "user/message"
                    and event.get("data", {}).get("text") == QUESTION
                    for event in events
                ):
                    parent_matches.append((path, events))
            if len(parent_matches) != 1:
                raise AssertionError(
                    f"expected one lifecycle parent session, found {len(parent_matches)}"
                )
            parent_path, parent_events = parent_matches[0]
            turn_starts = sum(
                event.get("kind") == "turn/start" for event in parent_events
            )
            turn_ends = sum(event.get("kind") == "turn/end" for event in parent_events)
            assistant_text = "\n".join(
                str(event.get("data", {}).get("content", ""))
                for event in parent_events
                if event.get("kind") == "assistant/message"
            )

            with request_lock:
                recorded = json.loads(json.dumps(requests))
            expected_models = [MAIN_MODEL, ADVISOR_MODEL, MAIN_MODEL]
            contracts = {
                "initially_disabled": any(
                    "›" in line and "No advisor" in line and "✔ current" in line
                    for line in initial_disabled_screen.splitlines()
                ),
                "selected_route_persisted": selected_disk
                == {
                    "mode": "enabled",
                    "owner": "native:openrouter",
                    "model": ADVISOR_MODEL,
                    "effort": "",
                },
                "disable_persisted": disabled_disk
                == {"mode": "disabled", "owner": "", "model": "", "effort": ""},
                "selected_route_restored": (
                    f"enabled: native:openrouter · {ADVISOR_MODEL}"
                    in selected_restart_screen
                ),
                "disabled_route_restored": "advisor disabled" in disabled_restart_screen,
                "exact_three_request_lifecycle": [
                    request["model"] for request in recorded
                ]
                == expected_models,
                "parent_exposed_advisor": len(recorded) == 3
                and "advisor" in recorded[0]["tool_names"],
                "advisor_received_current_question": len(recorded) == 3
                and recorded[1]["question_present"],
                "advisor_had_no_tools": len(recorded) == 3
                and recorded[1]["tool_count"] == 0,
                "parent_received_guidance": len(recorded) == 3
                and recorded[2]["guidance_present"],
                "same_parent_turn": turn_starts == 1 and turn_ends == 1,
                "final_committed": FINAL in assistant_text,
                "all_inference_loopback": bool(recorded)
                and all(request["loopback"] for request in recorded),
            }
            result.update(
                {
                    "status": "passed" if all(contracts.values()) else "gaps_observed",
                    "contract": contracts,
                    "provider_requests": recorded,
                    "selected_settings": selected_disk,
                    "disabled_settings": disabled_disk,
                    "session_log_count": len(logs),
                    "parent_session": str(parent_path.relative_to(home)),
                    "parent_turn_starts": turn_starts,
                    "parent_turn_ends": turn_ends,
                }
            )
            (output / "requests.json").write_text(json.dumps(recorded, indent=2))
            (output / "parent-session.jsonl").write_text(parent_path.read_text())
            (output / "settings-final.toml").write_text(settings_path.read_text())
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
    finally:
        if prior_key is None:
            os.environ.pop("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE", None)
        else:
            os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = prior_key
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        (output / "result.json").write_text(json.dumps(result, indent=2))

    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
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
