#!/usr/bin/env python3
"""Verify the canonical four-child settings shell through the real heycode TUI.

The journey uses a fresh immutable binary plus disposable HEYCODE_HOME/workspace.
It exercises only local slash commands; no model prompt or provider request is
submitted. Captures are terminal-cell screenshots produced from a real PTY.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import time

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


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


class Journey:
    def __init__(
        self,
        binary: Path,
        output: Path,
        home: Path,
        workspace: Path,
        *,
        columns: int,
        rows: int,
        timeout: float,
    ) -> None:
        self.output = output
        self.timeout = timeout
        self.screen = Screen(columns, rows)
        self.stream = TerminalByteStream(self.screen)
        self.tui = FullScreenTui(
            str(home),
            str(workspace),
            str(binary),
            fake=True,
            color=True,
            rows=rows,
            columns=columns,
        )
        self.captures: list[str] = []

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def read(self, seconds: float = 0.15) -> str:
        self.stream.feed(self.tui.read(seconds))
        return self.text()

    def wait(self, needle: str, timeout: float | None = None) -> str:
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

    def settle(self, seconds: float = 0.5) -> str:
        deadline = time.monotonic() + seconds
        latest = self.text()
        while time.monotonic() < deadline:
            latest = self.read(min(0.15, deadline - time.monotonic()))
        return latest

    def send(self, value: bytes, *, seconds: float = 0.2) -> str:
        os.write(self.tui.fd, value)
        return self.read(seconds)

    def command(self, value: str, expected: str) -> str:
        self.send(value.encode())
        self.send(b"\r")
        return self.wait(expected)

    def click_text(self, line_contains: str, label: str) -> str:
        matches = [
            (row, line.index(label))
            for row, line in enumerate(self.screen.display)
            if line_contains in line and label in line
        ]
        if not matches:
            raise AssertionError(
                f"no visible mouse target {label!r} on {line_contains!r}:\n{self.text()}"
            )
        row, column = matches[-1]
        self.send(f"\x1b[<0;{column + 1};{row + 1}M".encode())
        return self.settle()

    def capture(self, name: str) -> str:
        visible = self.settle()
        (self.output / f"{name}.txt").write_text(visible)
        (self.output / f"{name}.ansi").write_bytes(self.tui.transcript)
        render_screen(self.screen, self.output / f"{name}.png")
        self.captures.append(name)
        return visible

    def close_panel(self, *, config: bool = False) -> None:
        # Config starts in Search. An empty-query Esc deliberately moves to
        # Rows first, so it needs a second Esc; the read-only children close
        # from Tabs on the first press.
        self.send(b"\x1b")
        if config:
            self.send(b"\x1b")
        self.settle()

    def stop(self) -> None:
        if self.tui.alive():
            self.send(b"/quit\r")
            deadline = time.monotonic() + 10
            while self.tui.alive() and time.monotonic() < deadline:
                self.read(0.1)
        self.tui.close()


def assert_shell(text: str) -> None:
    for label in ("Status", "Config", "Usage", "Stats"):
        assert label in text, (label, text)


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

    with tempfile.TemporaryDirectory(prefix="heycode-settings-shell-pty-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        (home / "config.toml").write_text(
            'schema_version = 31\n\n[ui]\naccent = "#224466"\n'
        )

        journey = Journey(
            binary,
            output,
            home,
            workspace,
            columns=columns,
            rows=rows,
            timeout=timeout,
        )
        try:
            journey.wait("shift+tab to cycle")

            status = journey.command("/status", "runtime:")
            assert_shell(status)
            assert "workspace:" in status and "permission:" in status, status
            journey.capture("01-direct-status")
            assertions.append("/status opens the Status child with live local runtime facts")

            # Direct Status starts with Tabs focus. Right traverses every child
            # and wraps Stats -> Status without leaving the shell.
            journey.send(b"\x1b[C")
            config_from_tab = journey.wait("Search:")
            assert "settings" in config_from_tab, config_from_tab
            journey.capture("02-tab-config")
            journey.send(b"\x1b[C")
            usage_from_tab = journey.wait("usage")
            assert "turns:" in usage_from_tab and "reported tokens:" in usage_from_tab, usage_from_tab
            journey.capture("03-tab-usage")
            journey.send(b"\x1b[C")
            stats_from_tab = journey.wait("state: empty (no durable session activity yet)")
            journey.capture("04-tab-stats")
            journey.send(b"\x1b[C")
            wrapped_status = journey.wait("runtime:")
            assert "workspace:" in wrapped_status, wrapped_status
            journey.capture("05-tab-wrap-status")
            assertions.append("Right traverses Config, Usage and Stats, then wraps to Status")
            journey.close_panel()

            config = journey.command("/config", "Search:")
            assert_shell(config)
            assert "settings" in config, config
            journey.send(b"copy")
            filtered = journey.wait("Search: copy")
            assert "ui-preferences.copy_full_response" in filtered, filtered
            journey.capture("06-config-search")
            journey.send(b"\x1b[B")
            rows_focused = journey.wait("Enter/Space to change")
            assert "ui-preferences.copy_full_response" in rows_focused, rows_focused
            journey.capture("07-config-row-focus")
            journey.send(b"/")
            search_again = journey.wait("Type to filter")
            assert "Search:" in search_again, search_again
            journey.send(b"\x1b[A")
            tabs_focused = journey.wait("to switch")
            journey.capture("08-config-tab-focus")
            journey.send(b"\x1b[D")
            status_from_config = journey.wait("runtime:")
            assert "workspace:" in status_from_config, status_from_config
            journey.capture("09-config-left-status")
            journey.send(b"\x1b[D")
            wrapped_stats = journey.wait("state: empty (no durable session activity yet)")
            journey.capture("10-status-left-wrap-stats")
            assertions.append(
                "Config owns search, row and tab focus; slash returns to search and Left wraps Status to Stats"
            )
            journey.close_panel()

            usage = journey.command("/usage", "usage")
            assert_shell(usage)
            assert "turns:" in usage and "reported tokens:" in usage, usage
            journey.capture("11-direct-usage")
            journey.close_panel()

            stats = journey.command("/stats", "state: empty (no durable session activity yet)")
            assert_shell(stats)
            journey.capture("12-direct-stats")
            journey.close_panel()

            settings = journey.command("/settings", "Search:")
            assert_shell(settings)
            assert "settings" in settings, settings
            journey.capture("13-settings-canonical-config")
            stats_mouse = journey.click_text(
                "Status  Config  Usage  Stats", "Stats"
            )
            assert "state: empty (no durable session activity yet)" in stats_mouse, stats_mouse
            config_mouse = journey.click_text(
                "Status  Config  Usage  Stats", "Config"
            )
            assert "Search:" in config_mouse, config_mouse
            journey.click_text("Search:", "Search:")
            journey.send(b"copy")
            journey.wait("ui-preferences.copy_full_response")
            row_mouse = journey.click_text(
                "ui-preferences.copy_full_response",
                "ui-preferences.copy_full_response",
            )
            assert "Enter/Space to change" in row_mouse, row_mouse
            journey.capture("14-settings-mouse-tab-search-row")
            assertions.append(
                "/config, /usage, /stats and /settings enter their canonical selected children"
            )
            assertions.append(
                "mouse selects shell tabs, Config search and one row without committing it"
            )
            journey.send(b"\x1b")
            journey.settle()
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
            "assistant/message",
        }
        forbidden_events = [event for event in events if event.get("kind") in forbidden]
        assert forbidden_events == [], forbidden_events
        assertions.append(
            "all settings-shell commands completed with zero durable user, assistant or model-request events"
        )
        (output / "events.json").write_text(json.dumps(events, indent=2))
        (output / "config.toml").write_text((home / "config.toml").read_text())
        (output / "terminal.ansi").write_bytes(journey.tui.transcript)

    result: dict[str, object] = {
        "status": "passed",
        "runtime": "real heycode CLI full-screen TUI over PTY",
        "binary": str(binary),
        "binary_sha256": binary_hash,
        "provider": "built-in fake selected but never invoked",
        "external_provider_requests": 0,
        "durable_model_input_events": 0,
        "captures": journey.captures,
        "assertions": assertions,
        "evidence_boundary": (
            "Controlled local terminal acceptance. It proves the immutable binary's "
            "four-child shell and local command wiring, not live provider behavior."
        ),
    }
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=46)
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()
    try:
        result = run(
            args.binary,
            args.out,
            columns=args.columns,
            rows=args.rows,
            timeout=args.timeout,
        )
    except Exception as error:  # noqa: BLE001 - acceptance runner must report all failures
        print(f"FAIL: {type(error).__name__}: {error}", file=os.sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
