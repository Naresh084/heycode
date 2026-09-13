#!/usr/bin/env python3
"""Exercise MCP panel mouse, paste and narrow-height behavior over a real PTY.

The run uses an immutable heycode binary, a disposable home/workspace and two
credential-free local stdio fixtures. It never submits a model prompt and
does not connect to user-configured servers.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import tomllib

from settings_mcp_terminal_check import FIXTURE_SOURCE, Journey, read_json_lines, starts


def locate(
    journey: Journey,
    needle: str,
    *,
    line_contains: str | None = None,
) -> tuple[int, int]:
    """Return the one-based xterm coordinate of a visible control."""
    for row, line in enumerate(journey.screen.display):
        if needle in line and (line_contains is None or line_contains in line):
            return line.index(needle) + 1, row + 1
    raise AssertionError(
        f"could not find visible control {needle!r} in:\n{journey.text()}"
    )


def click(
    journey: Journey,
    needle: str,
    *,
    line_contains: str | None = None,
) -> str:
    column, row = locate(journey, needle, line_contains=line_contains)
    return journey.send(
        (
            f"\x1b[<0;{column};{row}M"
            f"\x1b[<0;{column};{row}m"
        ).encode(),
        read_seconds=0.35,
    )


def bracketed_paste(journey: Journey, text: str) -> str:
    return journey.send(
        b"\x1b[200~" + text.encode() + b"\x1b[201~",
        read_seconds=0.4,
    )


def wheel(journey: Journey, needle: str, *, down: bool) -> str:
    column, row = locate(journey, needle)
    button = 65 if down else 64
    return journey.send(
        f"\x1b[<{button};{column};{row}M".encode(),
        read_seconds=0.35,
    )


def read_events(home: Path) -> list[dict[str, object]]:
    events: list[dict[str, object]] = []
    for path in sorted(home.rglob("session.jsonl")):
        events.extend(read_json_lines(path))
    return events


def run(
    binary: Path,
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    binary = binary.resolve()
    if not binary.is_file():
        raise FileNotFoundError(binary)
    output.mkdir(parents=True, exist_ok=False)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    assertions: list[str] = []

    with tempfile.TemporaryDirectory(prefix="heycode-mcp-panel-pty-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        fixture = root / "fixture_server.py"
        fixture_log = root / "fixture.jsonl"
        fixture.write_text(FIXTURE_SOURCE)
        fixture.chmod(0o755)

        environment = os.environ.copy()
        environment["HEYCODE_HOME"] = str(home)
        seed_results: list[dict[str, object]] = []
        for name in ("alpha", "beta"):
            process = subprocess.run(
                [
                    str(binary),
                    "mcp",
                    "add",
                    name,
                    "--command",
                    sys.executable,
                    "--",
                    str(fixture),
                    str(fixture_log),
                ],
                cwd=workspace,
                env=environment,
                text=True,
                capture_output=True,
                timeout=30,
                check=False,
            )
            seed_results.append(
                {
                    "name": name,
                    "returncode": process.returncode,
                    "stdout": process.stdout,
                    "stderr": process.stderr,
                }
            )
            assert process.returncode == 0, seed_results[-1]
        (output / "seed-commands.json").write_text(json.dumps(seed_results, indent=2))

        settings = home / "settings.toml"
        seeded_settings = settings.read_bytes()
        journey = Journey(
            binary,
            output,
            home,
            workspace,
            fixture_log,
            columns=columns,
            rows=rows,
            timeout=timeout,
        )
        try:
            journey.start()
            journey.wait_for_starts(2)
            panel = journey.command("/mcp", "MCP servers")
            if "ready" not in panel:
                panel = journey.wait("ready")
            assert "alpha" in panel and "beta" in panel
            initial = journey.capture("01-narrow-panel")
            assert "esc close" in initial and "tab section" in initial
            assertions.append(
                "the real 76x22 panel preserves both configured rows and its keyboard hint"
            )

            # Pasted command text outside a form is consumed by the MCP modal.
            bracketed_paste(journey, "/quit\n")
            journey.settle(0.25)
            assert journey.tui is not None and journey.tui.alive()
            assert "MCP servers" in journey.text()
            assert settings.read_bytes() == seeded_settings
            journey.capture("02-panel-paste-consumed")
            assertions.append(
                "bracketed paste outside a form neither closed the panel nor mutated MCP settings"
            )

            # The real SGR wheel remains owned by the modal and follows the
            # focused server list. Return to alpha so the subsequent click is
            # independently observable.
            wheel(journey, "alpha", down=True)
            selected = journey.wait("● beta")
            assert "● beta" in selected
            assert settings.read_bytes() == seeded_settings
            journey.capture("03-wheel-selected-beta")
            wheel(journey, "alpha", down=False)
            journey.wait("● alpha")

            # Pointer clicks change only selection and section focus. Settings
            # stay byte-identical until an explicit form submission.
            click(journey, "beta")
            selected = journey.wait("● beta")
            assert "● beta" in selected
            assert settings.read_bytes() == seeded_settings
            journey.capture("04-mouse-selected-beta")

            click(journey, "actions", line_contains="status")
            focused = journey.wait("[actions]")
            assert "Add server" in focused
            assert settings.read_bytes() == seeded_settings
            journey.capture("05-mouse-actions-section")

            click(journey, "Add server")
            assert settings.read_bytes() == seeded_settings
            journey.send(b"\r")
            form = journey.wait("add server")
            assert "name" in form and "transport" in form and "command" in form
            assert "enter submit" in form and "esc cancel" in form
            journey.capture("06-narrow-add-form")
            assertions.append(
                "wheel and mouse stayed inside the panel; clicks selected the server, section and action without executing it; Enter alone opened the form"
            )

            # Empty explicit submission returns to the panel with a visible,
            # specific validation error and the narrow-height hint retained.
            journey.send(b"\r")
            error = journey.wait("server name must")
            assert "esc close" in error
            assert settings.read_bytes() == seeded_settings
            journey.capture("07-narrow-required-error")
            assertions.append(
                "a required-field error and escape hint remain visible at narrow height"
            )

            click(journey, "Add server")
            journey.send(b"\r")
            journey.wait("add server")
            click(journey, "command")
            bracketed_paste(journey, "/usr/bin/false\n")
            pasted_target = journey.wait("/usr/bin/false")
            assert journey.tui is not None and journey.tui.alive()
            assert settings.read_bytes() == seeded_settings
            assert "enter submit" in pasted_target
            journey.capture("08-mouse-target-paste")

            click(journey, "name")
            bracketed_paste(journey, "local\n")
            pasted_name = journey.wait("local")
            assert journey.tui is not None and journey.tui.alive()
            assert settings.read_bytes() == seeded_settings
            assert "enter submit" in pasted_name
            journey.capture("09-mouse-name-paste-no-submit")
            assertions.append(
                "mouse-focused bracketed paste filled only bounded form fields and did not submit"
            )

            journey.send(b"\r")
            committed = journey.wait("added `local`")
            assert "local" in committed and "MCP servers" in committed
            parsed = tomllib.loads(settings.read_text())
            servers = parsed["settings"]["mcp-servers"]["servers"]
            assert sorted(servers) == ["alpha", "beta", "local"]
            assert servers["local"]["transport"] == "stdio"
            assert servers["local"]["target"] == "/usr/bin/false"
            journey.capture("10-explicit-enter-committed")
            assertions.append(
                "a separate Enter committed the sanitized name and target through the real management store"
            )
        finally:
            journey.stop()

        events = read_events(home)
        forbidden_kinds = {
            "user/message",
            "turn/start",
            "request/header",
            "request/start",
        }
        forbidden = [event for event in events if event.get("kind") in forbidden_kinds]
        assert forbidden == [], forbidden
        lifecycle = read_json_lines(fixture_log)
        assert len(starts(fixture_log)) == 2, lifecycle
        assert not any(
            row.get("event") == "message" and row.get("method") == "tools/call"
            for row in lifecycle
        ), lifecycle
        (output / "fixture-server.py").write_text(FIXTURE_SOURCE)
        (output / "fixture-lifecycle.jsonl").write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in lifecycle)
        )
        (output / "events.json").write_text(json.dumps(events, indent=2))
        (output / "settings-final.toml").write_text(settings.read_text())
        (output / "terminal.ansi").write_bytes(journey.transcript)
        assertions.append(
            "the controlled run emitted no durable model-input events and no MCP tools/call"
        )

    result: dict[str, object] = {
        "status": "passed",
        "runtime": "real heycode CLI full-screen TUI over PTY",
        "binary": str(binary),
        "binary_sha256": binary_hash,
        "terminal": {"columns": columns, "rows": rows},
        "provider": "built-in fake selected but never invoked",
        "external_provider_requests": 0,
        "mcp_fixture": "two credential-free local stdio definitions only",
        "fixture_start_count": 2,
        "mcp_tool_calls": 0,
        "durable_model_input_events": 0,
        "captures": journey.captures,
        "assertions": assertions,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=76)
    parser.add_argument("--rows", type=int, default=22)
    parser.add_argument("--timeout", type=float, default=45)
    arguments = parser.parse_args()
    run(
        arguments.binary,
        arguments.output.resolve(),
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
