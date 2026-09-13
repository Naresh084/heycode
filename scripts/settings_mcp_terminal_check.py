#!/usr/bin/env python3
"""Verify /config and /mcp argument paths through the real heycode TUI.

The journey uses a fresh immutable binary, a disposable HEYCODE_HOME/workspace,
and a credential-free local stdio MCP fixture.  It never submits a model
prompt.  Captures come from terminal cells driven over a real PTY; settings,
fixture lifecycle events, and session journals are retained as independent
state evidence.
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

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


FIXTURE_SOURCE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

log_path = Path(sys.argv[1])

if log_path.is_file():
    launch = 1 + sum(
        1
        for line in log_path.read_text(errors="replace").splitlines()
        if line.strip() and json.loads(line).get("event") == "start"
    )
else:
    launch = 1

def record(value):
    with log_path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, separators=(",", ":")) + "\n")

record({"event": "start", "pid": os.getpid(), "launch": launch})
try:
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = message.get("method")
        record({"event": "message", "method": method, "id": message.get("id")})
        if "id" not in message:
            continue
        if method == "initialize":
            result = {
                "protocolVersion": message.get("params", {}).get(
                    "protocolVersion", "2024-11-05"
                ),
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": "terminal-local-fixture",
                    "version": str(launch),
                },
            }
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": f"launch_{launch}",
                        "description": "identifies the live fixture generation",
                        "inputSchema": {"type": "object"},
                    }
                ]
            }
        elif method == "resources/list":
            result = {"resources": []}
        elif method == "prompts/list":
            result = {"prompts": []}
        elif method == "ping":
            result = {}
        else:
            result = {}
        print(
            json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result},
                       separators=(",", ":")),
            flush=True,
        )
finally:
    record({"event": "stop", "pid": os.getpid()})
'''


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def read_json_lines(path: Path) -> list[dict[str, object]]:
    if not path.is_file():
        return []
    return [
        json.loads(line)
        for line in path.read_text(errors="replace").splitlines()
        if line.strip()
    ]


def starts(path: Path) -> list[dict[str, object]]:
    return [row for row in read_json_lines(path) if row.get("event") == "start"]


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except (OSError, ValueError):
        return False
    return True


def wait_for_pid_exit(pid: int, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not pid_alive(pid):
            return
        time.sleep(0.025)
    raise AssertionError(f"fixture PID {pid} remained alive after reconnect")


def enabled_state(settings: str) -> bool:
    try:
        value = tomllib.loads(settings)["settings"]["mcp-servers"]["servers"][
            "fixture"
        ]["enabled"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise AssertionError(
            f"could not resolve fixture enabled state:\n{settings}"
        ) from error
    if not isinstance(value, bool):
        raise AssertionError(f"fixture enabled state is not boolean: {value!r}")
    return value


class Journey:
    def __init__(
        self,
        binary: Path,
        output: Path,
        home: Path,
        workspace: Path,
        fixture_log: Path,
        *,
        columns: int,
        rows: int,
        timeout: float,
    ) -> None:
        self.binary = binary
        self.output = output
        self.home = home
        self.workspace = workspace
        self.fixture_log = fixture_log
        self.columns = columns
        self.rows = rows
        self.timeout = timeout
        self.tui: FullScreenTui | None = None
        self.screen = Screen(columns, rows)
        self.stream = TerminalByteStream(self.screen)
        self.transcript = bytearray()
        self.captures: list[str] = []

    def start(self, *, resume: bool = False) -> None:
        assert self.tui is None
        self.screen = Screen(self.columns, self.rows)
        self.stream = TerminalByteStream(self.screen)
        self.tui = FullScreenTui(
            str(self.home),
            str(self.workspace),
            str(self.binary),
            fake=True,
            color=True,
            rows=self.rows,
            columns=self.columns,
            extra=["--continue"] if resume else [],
        )
        self.wait("shift+tab to cycle")

    def read(self, seconds: float = 0.15) -> str:
        assert self.tui is not None
        self.stream.feed(self.tui.read(seconds))
        return self.text()

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def wait(self, needle: str, timeout: float | None = None) -> str:
        assert self.tui is not None
        deadline = time.monotonic() + (timeout or self.timeout)
        latest = self.text()
        while time.monotonic() < deadline:
            latest = self.read()
            if needle in latest:
                return latest
            if not self.tui.alive():
                raise AssertionError(
                    f"CLI exited while waiting for {needle!r}:\n{latest}"
                )
        raise AssertionError(f"timed out waiting for {needle!r}:\n{latest}")

    def wait_for_starts(self, count: int) -> list[dict[str, object]]:
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            self.read()
            current = starts(self.fixture_log)
            if len(current) >= count:
                return current
        raise AssertionError(
            f"fixture did not reach {count} starts: {read_json_lines(self.fixture_log)}"
        )

    def send(self, value: bytes, *, read_seconds: float = 0.2) -> str:
        assert self.tui is not None
        os.write(self.tui.fd, value)
        return self.read(read_seconds)

    def command(self, value: str, expected: str) -> str:
        self.send(value.encode())
        self.send(b"\r")
        return self.wait(expected)

    def styled_fingerprint(self) -> tuple[tuple[object, ...], ...]:
        return tuple(
            (
                row,
                column,
                cell.data,
                cell.fg,
                cell.bg,
                cell.bold,
                cell.reverse,
            )
            for row in range(self.rows)
            for column in range(self.columns)
            if (cell := self.screen.buffer[row][column]).data.strip()
        )

    def capture(self, name: str) -> str:
        visible = self.read(0.35)
        (self.output / f"{name}.txt").write_text(visible)
        assert self.tui is not None
        (self.output / f"{name}.ansi").write_bytes(self.tui.transcript)
        render_screen(self.screen, self.output / f"{name}.png")
        self.captures.append(name)
        return visible

    def settle(self, seconds: float) -> None:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.read(min(0.15, deadline - time.monotonic()))

    def stop(self) -> None:
        if self.tui is None:
            return
        tui = self.tui
        if tui.alive():
            self.send(b"/quit\r")
            deadline = time.monotonic() + 10
            while tui.alive() and time.monotonic() < deadline:
                self.read(0.1)
        self.transcript.extend(tui.transcript)
        tui.close()
        self.tui = None


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

    with tempfile.TemporaryDirectory(prefix="heycode-config-mcp-pty-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        config = home / "config.toml"
        settings = home / "settings.toml"
        fixture = root / "fixture_server.py"
        fixture_log = root / "fixture.jsonl"
        config.write_text('schema_version = 31\n\n[ui]\naccent = "#224466"\n')
        fixture.write_text(FIXTURE_SOURCE)
        fixture.chmod(0o755)

        environment = os.environ.copy()
        environment["HEYCODE_HOME"] = str(home)
        seed = subprocess.run(
            [
                str(binary),
                "mcp",
                "add",
                "fixture",
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
        (output / "seed-command.json").write_text(
            json.dumps(
                {
                    "argv": [
                        str(binary),
                        "mcp",
                        "add",
                        "fixture",
                        "--command",
                        sys.executable,
                        "--",
                        "<disposable-fixture>",
                        "<disposable-log>",
                    ],
                    "returncode": seed.returncode,
                    "stdout": seed.stdout,
                    "stderr": seed.stderr,
                },
                indent=2,
            )
        )
        assert seed.returncode == 0, (seed.stdout, seed.stderr)
        assert settings.is_file(), list(home.iterdir())
        settings_seeded = settings.read_text()
        assert enabled_state(settings_seeded)
        (output / "settings-00-seeded.toml").write_text(settings_seeded)

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
            first_starts = journey.wait_for_starts(1)
            first_pid = int(first_starts[0]["pid"])
            assert pid_alive(first_pid)
            journey.command("/rename Config MCP PTY", "Renamed to Config MCP PTY")

            journey.command("/help", "Help")
            journey.send(b"\t")
            journey.wait("Search commands:")
            journey.send(b"config")
            config_help = journey.wait("/config [action]")
            assert "Open settings or show effective configuration provenance" in config_help
            journey.capture("01-help-config-metadata")
            journey.send(b"\x7f" * len("config"))
            journey.send(b"mcp")
            mcp_help = journey.wait("/mcp [action] [server]")
            assert "Open or manage MCP servers" in mcp_help
            journey.capture("02-help-mcp-metadata")
            journey.send(b"\x1b")
            journey.wait("Config MCP PTY")
            assertions.append("real help exposes exact /config and /mcp synopses and descriptions")

            journey.command("/config", "Settings")
            journey.capture("03-config-settings-panel")
            initial_style = journey.styled_fingerprint()
            journey.send(b"\x1b[B")
            journey.settle(0.4)
            selected_style = journey.styled_fingerprint()
            assert selected_style != initial_style
            journey.capture("04-config-settings-keyboard")
            assertions.append("bare /config opens Settings and keyboard selection repaints")
            journey.send(b"\x1b")
            journey.wait("Config MCP PTY")

            config_view = journey.command("/config show", '#224466')
            assert "ui.accent" in config_view
            assert str(config) in config_view
            journey.capture("05-config-show-provenance")
            before_mouse = journey.text()
            for _ in range(6):
                journey.send(b"\x1b[<64;63;22M", read_seconds=0.08)
            journey.settle(0.4)
            after_mouse = journey.text()
            assert after_mouse != before_mouse
            journey.capture("06-config-show-mouse-scroll")
            assertions.append("/config show renders value provenance and real mouse-wheel scrolling")

            journey.command("/config unexpected", "usage: /config [show]")
            journey.capture("07-config-invalid-argument")
            assertions.append("invalid /config arguments fail locally with exact usage")

            mcp = journey.command("/mcp", "MCP servers")
            if "fixture" not in mcp or "ready" not in mcp:
                mcp = journey.wait("ready")
            assert "fixture" in mcp and "enabled" in mcp
            journey.capture("08-mcp-enabled-ready-panel")
            journey.send(b"\x1b")
            journey.wait("Config MCP PTY")
            assertions.append("bare /mcp opens the enabled ready local fixture panel")

            before_reconnect = settings.read_bytes()
            journey.command(
                "/mcp reconnect fixture",
                "started bounded reconnect for MCP server `fixture`",
            )
            second_starts = journey.wait_for_starts(2)
            second_pid = int(second_starts[1]["pid"])
            assert second_pid != first_pid and pid_alive(second_pid)
            wait_for_pid_exit(first_pid, timeout)
            assert settings.read_bytes() == before_reconnect
            reconnected_panel = journey.command("/mcp", "MCP servers")
            if "generation 2" not in reconnected_panel:
                reconnected_panel = journey.wait("generation 2")
            assert "terminal-local-fixture 2" in reconnected_panel
            journey.capture("09-mcp-reconnect-ready-generation")
            journey.send(b"\x1b")
            journey.wait("Config MCP PTY")
            assertions.append(
                "manual reconnect replaced exactly the live fixture, published generation 2, retired the old PID, and left settings unchanged"
            )

            journey.command(
                "/mcp disable fixture",
                "current live connections are unchanged until MCP connections are recomposed",
            )
            disabled_settings = settings.read_text()
            assert not enabled_state(disabled_settings)
            assert pid_alive(second_pid)
            assert len(starts(fixture_log)) == 2
            (output / "settings-01-disabled.toml").write_text(disabled_settings)
            journey.capture("10-mcp-disable-receipt")
            assertions.append("disable persists false while the current live fixture PID stays alive")

            before_disabled_reconnect = settings.read_bytes()
            journey.command(
                "/mcp reconnect fixture",
                "MCP server `fixture` is disabled",
            )
            assert settings.read_bytes() == before_disabled_reconnect
            assert pid_alive(second_pid)
            assert len(starts(fixture_log)) == 2
            journey.capture("11-mcp-reconnect-disabled-refusal")
            assertions.append("reconnect refuses a disabled definition without mutating settings or process state")

            journey.command(
                "/mcp restart fixture",
                "usage: /mcp [enable|disable|reconnect] <server>",
            )
            journey.capture("12-mcp-invalid-argument")
            assertions.append("invalid /mcp action fails locally with exact usage")
            journey.stop()

            journey.start(resume=True)
            journey.settle(2.0)
            assert len(starts(fixture_log)) == 2
            disabled_panel = journey.command("/mcp", "MCP servers")
            assert "fixture" in disabled_panel and "disabled" in disabled_panel
            journey.capture("13-restart-disabled-panel")
            journey.send(b"\x1b")
            journey.wait("Config MCP PTY")
            journey.command(
                "/mcp enable fixture",
                "current live connections are unchanged until MCP connections are recomposed",
            )
            enabled_settings = settings.read_text()
            assert enabled_state(enabled_settings)
            assert len(starts(fixture_log)) == 2
            (output / "settings-02-enabled.toml").write_text(enabled_settings)
            journey.capture("14-mcp-enable-receipt")
            assertions.append("disabled state suppresses launch after restart; enable persists without hot-start")
            journey.stop()

            journey.start(resume=True)
            third_starts = journey.wait_for_starts(3)
            third_pid = int(third_starts[2]["pid"])
            assert third_pid not in {first_pid, second_pid} and pid_alive(third_pid)
            ready_panel = journey.command("/mcp", "MCP servers")
            if "ready" not in ready_panel:
                ready_panel = journey.wait("ready")
            assert "fixture" in ready_panel and "enabled" in ready_panel
            journey.capture("15-restart-enabled-ready-panel")
            assertions.append("recomposition applies persisted enable and launches one new ready fixture")
            journey.stop()
        finally:
            journey.stop()

        events: list[dict[str, object]] = []
        event_paths = sorted(home.rglob("session.jsonl"))
        for path in event_paths:
            events.extend(read_json_lines(path))
        forbidden = {
            "user/message",
            "turn/start",
            "request/header",
            "request/start",
        }
        forbidden_events = [event for event in events if event.get("kind") in forbidden]
        assert forbidden_events == [], forbidden_events
        title_events = [event for event in events if event.get("kind") == "session/title"]
        assert len(title_events) == 1, title_events
        assertions.append("all slash commands completed with zero durable model request or user-message events")

        lifecycle = read_json_lines(fixture_log)
        assert len(starts(fixture_log)) == 3, lifecycle
        assert not any(
            row.get("event") == "message" and row.get("method") == "tools/call"
            for row in lifecycle
        ), lifecycle
        (output / "fixture-server.py").write_text(FIXTURE_SOURCE)
        (output / "fixture-lifecycle.jsonl").write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in lifecycle)
        )
        (output / "events.json").write_text(json.dumps(events, indent=2))
        (output / "config.toml").write_text(config.read_text())
        (output / "terminal.ansi").write_bytes(journey.transcript)

    result: dict[str, object] = {
        "status": "passed",
        "runtime": "real heycode CLI TUI over PTY",
        "binary": str(binary),
        "binary_sha256": binary_hash,
        "provider": "built-in fake selected but never invoked",
        "external_provider_requests": 0,
        "fixture": "credential-free local stdio MCP server",
        "fixture_start_count": 3,
        "durable_model_input_events": 0,
        "captures": journey.captures,
        "assertions": assertions,
        "mouse_boundary": (
            "mouse evidence is transcript wheel scrolling; Settings and MCP panels "
            "currently expose keyboard handling only"
        ),
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=126)
    parser.add_argument("--rows", type=int, default=48)
    parser.add_argument("--timeout", type=float, default=45)
    arguments = parser.parse_args()
    run(
        arguments.binary,
        arguments.output.resolve(),
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
