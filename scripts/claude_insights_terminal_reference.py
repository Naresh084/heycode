#!/usr/bin/env python3
"""Capture current Claude `/insights` UI through deterministic loopback I/O.

Disposable Claude journals contain only three synthetic turns. Every facet and
report-section request is forced to a 127.0.0.1 Anthropic-shaped fixture with a
dummy key. Complete, provider-failure, and Escape scenarios therefore exercise
the real current command implementation without a paid request or transcript
scan outside the fixture.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import uuid

from claude_session_info_terminal_reference import (
    ClaudePty,
    SYNTHETIC_MARKERS,
    seed_session,
    sha256,
)


SECTION_RESPONSES: dict[str, object] = {
    "facets": {
        "underlying_goal": "Finish a local command audit without paid provider traffic",
        "goal_categories": {"implement_feature": 1},
        "outcome": "fully_achieved",
        "user_satisfaction_counts": {"likely_satisfied": 1},
        "claude_helpfulness": "very_helpful",
        "session_type": "single_task",
        "friction_counts": {},
        "friction_detail": "",
        "primary_success": "correct_code_edits",
        "brief_summary": "The local command audit used controlled fixture evidence successfully.",
    },
    "project_areas": {
        "areas": [
            {
                "name": "Local command validation",
                "session_count": 1,
                "description": "You validated command behavior with isolated terminal fixtures and explicit network boundaries.",
            }
        ]
    },
    "interaction_style": {
        "narrative": "You use evidence-led, bounded command checks. You keep synthetic and live-provider claims separate.",
        "key_pattern": "You require inspectable local evidence before closure.",
    },
    "what_works": {
        "intro": "The fixture-driven workflow keeps command evidence reproducible.",
        "impressive_workflows": [
            {
                "title": "Bounded terminal evidence",
                "description": "You pair real terminal state with deterministic local transport responses.",
            }
        ],
    },
    "friction_analysis": {
        "intro": "The main friction is keeping source and native semantics distinct.",
        "categories": [
            {
                "category": "Semantic boundaries",
                "description": "A local equivalent can intentionally omit hosted inference.",
                "examples": [
                    "Structural aggregation does not inspect message text.",
                    "Controlled fixtures do not validate paid model quality.",
                ],
            }
        ],
    },
    "suggestions": {
        "claude_md_additions": [
            {
                "addition": "Keep paid-provider validation separate from deterministic command tests.",
                "why": "This boundary recurs across command validation.",
                "prompt_scaffold": "Add under the testing policy.",
            }
        ],
        "features_to_try": [
            {
                "feature": "Custom Skills",
                "one_liner": "Package repeatable validation instructions.",
                "why_for_you": "The audit sequence repeats across commands.",
                "example_code": "/validate-command",
            }
        ],
        "usage_patterns": [
            {
                "title": "Record evidence boundaries",
                "suggestion": "State what each fixture proves.",
                "detail": "This keeps controlled transport evidence from being mistaken for live-provider validation.",
                "copyable_prompt": "Separate implementation, controlled validation, and live validation.",
            }
        ],
    },
    "on_the_horizon": {
        "intro": "More capable models can broaden the same bounded validation pattern.",
        "opportunities": [
            {
                "title": "Parallel command-state validation",
                "whats_possible": "Independent state matrices can be reconciled against one acceptance contract.",
                "how_to_try": "Keep one immutable binary and isolated fixture per command family.",
                "copyable_prompt": "Validate these command states independently and reconcile only evidence-backed results.",
            }
        ],
    },
    "fun_ending": {
        "headline": "The entire rich report came from a loopback fixture",
        "detail": "No commercial provider received the synthetic transcript.",
    },
    "at_a_glance": {
        "whats_working": "You use deterministic terminal evidence and keep claims narrow.",
        "whats_hindering": "Source analytics and local structural reports intentionally answer different questions.",
        "quick_wins": "Keep exact request manifests beside the visual captures.",
        "ambitious_workflows": "Automate independent state matrices while retaining one explicit acceptance owner.",
    },
}


def classify_request(wire: str) -> str:
    if "The user just ran /insights" in wire:
        return "receipt"
    if "extract structured facets" in wire:
        return "facets"
    if "identify project areas" in wire:
        return "project_areas"
    if "describe the user's interaction style" in wire:
        return "interaction_style"
    if "what's working well" in wire:
        return "what_works"
    if "identify friction points" in wire:
        return "friction_analysis"
    if "suggest improvements" in wire:
        return "suggestions"
    if "identify future opportunities" in wire:
        return "on_the_horizon"
    if "find a memorable moment" in wire:
        return "fun_ending"
    return "at_a_glance"


def run(output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    requests: list[dict[str, object]] = []
    request_lock = threading.Lock()
    state_lock = threading.Lock()
    state = {"scenario": "idle", "mode": "complete"}
    entered = threading.Event()
    release = threading.Event()
    settled = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def json_response(self, status: int, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            body = json.loads(raw or b"{}")
            if "count_tokens" in self.path:
                with request_lock:
                    requests.append(
                        {"path": self.path, "kind": "count_tokens", "loopback": True}
                    )
                self.json_response(200, {"input_tokens": 1000})
                return

            with state_lock:
                scenario = state["scenario"]
                mode = state["mode"]
            wire = raw.decode(errors="replace")
            section = classify_request(wire)
            record: dict[str, object] = {
                "path": self.path,
                "kind": "messages",
                "scenario": scenario,
                "mode": mode,
                "section": section,
                "loopback": True,
                "request_bytes": len(raw),
                "request_sha256": hashlib.sha256(raw).hexdigest(),
                "model": body.get("model"),
                "message_count": len(body.get("messages", [])),
                "tool_count": len(body.get("tools", [])),
                "synthetic_markers_present": [
                    marker for marker in SYNTHETIC_MARKERS if marker in wire
                ],
                "client_disconnected": False,
            }
            with request_lock:
                requests.append(record)
            entered.set()

            try:
                if mode == "failure":
                    self.json_response(
                        400,
                        {
                            "type": "error",
                            "error": {
                                "type": "invalid_request_error",
                                "message": f"LOCAL_INSIGHTS_FAILURE_{section}",
                            },
                        },
                    )
                    settled.set()
                    return

                if mode == "complete" and not release.is_set() and not release.wait(timeout=timeout):
                    self.json_response(
                        408,
                        {
                            "type": "error",
                            "error": {
                                "type": "timeout_error",
                                "message": "LOCAL_INSIGHTS_RELEASE_TIMEOUT",
                            },
                        },
                    )
                    settled.set()
                    return

                if section == "receipt":
                    report_url = re.search(r"Report URL: (.+?)\\nHTML file:", wire)
                    response = (
                        "Your shareable insights report is ready:\n"
                        f"{report_url.group(1) if report_url else 'local report'}\n"
                        "Want to dig into any section or try one of the suggestions?"
                    )
                else:
                    response = json.dumps(
                        SECTION_RESPONSES[section], separators=(",", ":")
                    )
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()

                def event(kind: str, value: object) -> None:
                    self.wfile.write(
                        f"event: {kind}\ndata: {json.dumps(value)}\n\n".encode()
                    )

                event(
                    "message_start",
                    {
                        "type": "message_start",
                        "message": {
                            "id": f"msg_local_insights_{scenario}_{section}",
                            "type": "message",
                            "role": "assistant",
                            "model": "claude-haiku-local-fixture",
                            "content": [],
                            "stop_reason": None,
                            "stop_sequence": None,
                            "usage": {
                                "input_tokens": 1000,
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": 0,
                                "output_tokens": 0,
                            },
                        },
                    },
                )
                event(
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""},
                    },
                )
                event(
                    "content_block_delta",
                    {
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": response},
                    },
                )
                event("content_block_stop", {"type": "content_block_stop", "index": 0})
                event(
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": None},
                        "usage": {"output_tokens": 100},
                    },
                )
                event("message_stop", {"type": "message_stop"})
                self.wfile.flush()
                if section == "receipt":
                    settled.set()
            except (BrokenPipeError, ConnectionResetError, socket.timeout):
                record["client_disconnected"] = True
                settled.set()

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    result: dict[str, object] = {
        "status": "failed",
        "scope": "Claude Code insights UI with synthetic local-only usage data",
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
            with state_lock:
                state["scenario"] = scenario
                state["mode"] = "failure" if scenario == "failure" else "complete"

            with tempfile.TemporaryDirectory(prefix=f"claude-insights-{scenario}-") as folder:
                root = Path(folder)
                sandbox_home = root / "home"
                config = root / "config"
                workspace = root / "workspace"
                for path in (sandbox_home, config, workspace):
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
                        key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_"))
                        or key.endswith("_API_KEY")
                        or key.endswith("_AUTH_TOKEN")
                    ):
                        environment.pop(key)
                environment.update(
                    {
                        "HOME": str(sandbox_home),
                        "TERM": "xterm-256color",
                        "COLORTERM": "truecolor",
                        "CLAUDE_CONFIG_DIR": str(config),
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                        "CLAUDE_CODE_REMOTE_CONTROL": "0",
                        "CLAUDE_CODE_NO_FLICKER": "1",
                        "BROWSER": "/usr/bin/false",
                        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                        "ANTHROPIC_API_KEY": "local-only-dummy",
                    }
                )
                common = [
                    "--safe-mode",
                    "--restricted",
                    "--strict-mcp-config",
                    "--no-chrome",
                    "--setting-sources",
                    "project,local",
                    "--settings",
                    '{"remoteControlAtStartup":false}',
                    "--permission-mode",
                    "manual",
                    "--model",
                    "opus",
                ]
                session_id = str(uuid.uuid4())
                bootstrap = ClaudePty(
                    executable,
                    environment,
                    workspace,
                    [*common, "--session-id", session_id, "--name", f"insights-{scenario}"],
                    columns=columns,
                    rows=rows,
                )
                try:
                    bootstrap.ready()
                    bootstrap.send(b"/copy\r")
                    bootstrap.wait_for("No assistant message to copy", timeout=20)
                    bootstrap.send(b"/exit\r")
                    deadline = time.monotonic() + 5
                    while bootstrap.process.poll() is None and time.monotonic() < deadline:
                        bootstrap.read()
                finally:
                    bootstrap.close()
                    (scenario_output / "bootstrap.ansi").write_bytes(bootstrap.transcript)

                logs = list(config.glob(f"projects/**/{session_id}.jsonl"))
                assert len(logs) == 1, (scenario, logs)
                log = logs[0]
                seed_session(log, session_id, workspace)
                seeded = log.read_bytes()
                (scenario_output / "seeded-session.jsonl").write_bytes(seeded)

                terminal = ClaudePty(
                    executable,
                    environment,
                    workspace,
                    [*common, "--resume", session_id],
                    columns=columns,
                    rows=rows,
                )
                ui_evidence: dict[str, bool] = {}
                try:
                    terminal.ready()
                    terminal.capture(scenario_output, "00-ready-with-synthetic-history")
                    terminal.send(b"/insights\r")
                    terminal.wait_event(entered, "first localhost insights request", timeout)
                    held = terminal.capture(scenario_output, "01-working")
                    ui_evidence["working_progress"] = (
                        "analyzing your sessions" in held.lower()
                        or "insight" in held.lower()
                    )

                    if scenario == "complete":
                        release.set()
                        terminal.wait_event(settled, "insights report completion", timeout)
                        completed = terminal.wait_for(
                            "Claude Code Insights", "At a Glance", "shareable insights", timeout=30
                        )
                        terminal.capture(scenario_output, "02-completed")
                        ui_evidence["completed_report_summary"] = (
                            "At a Glance" in completed
                            or "shareable insights" in completed.lower()
                        )
                    elif scenario == "failure":
                        terminal.wait_for(
                            "No insights generated",
                            "Couldn't",
                            "LOCAL_INSIGHTS_FAILURE",
                            "shareable insights",
                            timeout=60,
                        )
                        failed = terminal.capture(scenario_output, "02-provider-failure")
                        ui_evidence["provider_failure_or_degraded_result"] = any(
                            needle in failed.lower()
                            for needle in ("no insights", "couldn't", "local_insights_failure")
                        )
                    else:
                        terminal.send(b"\x1b")
                        terminal.read(0.8)
                        release.set()
                        terminal.read(1.0)
                        cancelled = terminal.capture(scenario_output, "02-cancelled")
                        ui_evidence["cancel_restores_command"] = (
                            "/insights" in cancelled
                            and "analyzing your sessions" not in cancelled.lower()
                        )
                finally:
                    (scenario_output / "terminal.ansi").write_bytes(terminal.transcript)
                    terminal.close()

                final = log.read_bytes()
                assert final.startswith(seeded), scenario
                (scenario_output / "final-session.jsonl").write_bytes(final)
                reports = list((config / "usage-data").glob("report*.html"))
                for report in reports:
                    (scenario_output / report.name).write_bytes(report.read_bytes())
                scenario_requests = [
                    request
                    for request in requests
                    if request.get("scenario") == scenario
                    and request.get("kind") == "messages"
                ]
                result["scenarios"][scenario] = {
                    "message_requests": len(scenario_requests),
                    "sections": [request["section"] for request in scenario_requests],
                    "seeded_prefix_byte_exact": True,
                    "report_files": [report.name for report in reports],
                    "ui_evidence": ui_evidence,
                    "client_disconnects": sum(
                        bool(request.get("client_disconnected"))
                        for request in scenario_requests
                    ),
                }

        failures = [
            f"{scenario}.{check}"
            for scenario, value in result["scenarios"].items()
            for check, passed in value["ui_evidence"].items()
            if not passed
        ]
        result.update(
            {
                "status": "passed" if not failures else "gaps_observed",
                "contract_failures": failures,
                "claude_version": subprocess.run(
                    [executable, "--version"],
                    capture_output=True,
                    text=True,
                    check=True,
                ).stdout.strip(),
                "claude_binary": str(Path(executable).resolve()),
                "claude_binary_sha256": sha256(Path(executable).resolve()),
                "localhost_requests": len(requests),
                "message_requests": sum(
                    request.get("kind") == "messages" for request in requests
                ),
                "count_token_requests": sum(
                    request.get("kind") == "count_tokens" for request in requests
                ),
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


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=50)
    parser.add_argument("--timeout", type=float, default=30)
    arguments = parser.parse_args()
    outcome = run(
        arguments.output,
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
    raise SystemExit(0 if outcome["status"] == "passed" else 1)
