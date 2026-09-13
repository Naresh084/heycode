#!/usr/bin/env python3
"""Exercise heycode /focus and /color through the real CLI and a PTY.

The run uses fake inference and disposable home/workspace directories. It
proves the UI-only command path, focus persistence, session-local color reset,
and durable title replay without contacting an external provider.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import tempfile
import time
from pathlib import Path

import pyte

from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(
    binary: Path,
    output: Path,
    *,
    theme: str,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    binary = binary.resolve()
    if not binary.is_file():
        raise FileNotFoundError(binary)
    output.mkdir(parents=True, exist_ok=True)
    captures: list[str] = []
    assertions: list[str] = []
    prompt_colors: dict[str, str] = {}

    with tempfile.TemporaryDirectory(prefix="heycode-focus-home-") as home:
        with tempfile.TemporaryDirectory(prefix="heycode-focus-work-") as workspace:
            Path(home, "settings.toml").write_text(
                "schema_version = 1\n"
                "[settings.ui-preferences]\n"
                f'theme = "{theme}"\n'
            )

            def start(*extra: str) -> tuple[FullScreenTui, Screen, pyte.ByteStream]:
                tui = FullScreenTui(
                    home,
                    workspace,
                    str(binary),
                    fake=True,
                    rows=rows,
                    columns=columns,
                    color=True,
                    extra=list(extra),
                )
                screen = Screen(columns, rows)
                return tui, screen, pyte.ByteStream(screen)

            def prompt_foreground(screen: Screen) -> str:
                matches: list[str] = []
                for y, line in enumerate(screen.display):
                    for x, character in enumerate(line):
                        if character == "❯":
                            matches.append(str(screen.buffer[y][x].fg))
                if not matches:
                    raise AssertionError("composer prompt glyph is missing")
                return matches[-1]

            def drive(
                tui: FullScreenTui,
                screen: Screen,
                stream: pyte.ByteStream,
                phase: str,
            ):
                def read(seconds: float = 0.1) -> str:
                    stream.feed(tui.read(seconds))
                    return "\n".join(screen.display)

                def send(text: str) -> None:
                    os.write(tui.fd, text.encode())
                    read(0.2)
                    os.write(tui.fd, b"\r")

                def wait_for(*needles: str, seconds: float | None = None) -> str:
                    deadline = time.monotonic() + (seconds or timeout)
                    latest = ""
                    while time.monotonic() < deadline:
                        latest = read()
                        if all(needle in latest for needle in needles):
                            return latest
                        if not tui.alive():
                            raise AssertionError(
                                f"CLI exited while waiting for {needles}:\n{latest}"
                            )
                    raise AssertionError(f"Missing {needles}:\n{latest}")

                def capture(name: str) -> str:
                    visible = read(0.35)
                    (output / f"{name}.txt").write_text(visible)
                    (output / f"{name}.ansi").write_bytes(tui.transcript)
                    render_screen(
                        screen,
                        output / f"{name}.png",
                        background="#f8f9fb" if theme == "heycode-light" else "#101014",
                        foreground="#20242c" if theme == "heycode-light" else "#e8eaf0",
                    )
                    captures.append(name)
                    return visible

                wait_for("shift+tab")
                return read, send, wait_for, capture

            tui, screen, stream = start()
            try:
                read, send, wait_for, capture = drive(tui, screen, stream, "initial")
                send("/focus")
                wait_for("Focus view enabled")
                capture("00-empty-focus")
                send("/focus")
                wait_for("Focus view disabled")
                send("/focus invalid")
                wait_for("usage: /focus")
                capture("00-invalid-focus")
                assertions.append("empty focus toggles locally and invalid arguments fail with usage")
                send("PHASE2_FOCUS_COMPLETE")
                wait_for("FAKE-REPLY", seconds=30)
                send("/rename Phase 2 Focus Session")
                wait_for("Renamed to Phase 2 Focus Session")
                normal = capture("01-normal-completed-turn")
                assert "PHASE2_FOCUS_COMPLETE" in normal and "FAKE-REPLY" in normal
                assert "Phase 2 Focus Session" in normal
                assertions.append("normal view retains the completed turn and durable title")

                send("/focus")
                focus = wait_for("Focus view enabled")
                capture("02-focus-view")
                assert "FAKE-REPLY" in focus and "Focus view enabled" in focus
                assertions.append("focus view keeps the current prompt, answer, and command state")

                send("/focus")
                restored = wait_for("Focus view disabled")
                restored = capture("03-normal-restored")
                assert "PHASE2_FOCUS_COMPLETE" in restored and "FAKE-REPLY" in restored
                assertions.append("second /focus restores the full transcript")

                send("/color")
                random_view = wait_for("Session color set to:")
                capture("04-color-random")
                random_match = re.search(
                    r"Session color set to: (red|blue|green|yellow|purple|orange|pink|cyan)",
                    random_view,
                )
                assert random_match is not None
                prompt_colors["random"] = prompt_foreground(screen)

                send("/color cyan")
                wait_for("Session color set to: cyan")
                capture("05-color-cyan")
                prompt_colors["cyan"] = prompt_foreground(screen)

                send("/color invalid")
                wait_for("usage: /color")
                capture("05-color-invalid-preserves-cyan")
                assert prompt_foreground(screen) == prompt_colors["cyan"]
                assertions.append("invalid color reports usage without changing the active accent")
                send("/color default")
                wait_for("Session color reset to default")
                capture("06-color-default")
                prompt_colors["default"] = prompt_foreground(screen)
                assert prompt_colors["cyan"] != prompt_colors["default"]
                assertions.append("random, cyan, and default update the live prompt-bar accent")

                send("/focus")
                wait_for("Focus view enabled")
                send("/color cyan")
                wait_for("Session color set to: cyan")
                capture("07-before-restart-focus-cyan")
                prompt_colors["before_restart"] = prompt_foreground(screen)
            finally:
                tui.close()

            resumed, resumed_screen, resumed_stream = start("--continue")
            try:
                read, send, wait_for, capture = drive(
                    resumed, resumed_screen, resumed_stream, "resume"
                )
                resumed_view = wait_for("PHASE2_FOCUS_COMPLETE", "FAKE-REPLY")
                capture("08-restart-focus-persists-color-resets")
                prompt_colors["after_restart"] = prompt_foreground(resumed_screen)
                assert "Phase 2 Focus Session" in resumed_view
                assert prompt_colors["after_restart"] != prompt_colors["before_restart"]
                send("/focus")
                wait_for("Focus view disabled")
                capture("09-restart-focus-toggle-proves-startup-state")
                assertions.append("restart replays the title and persisted focus preference")
                assertions.append("restart resets the session-local color to the theme default")
            finally:
                resumed.close()

            events: list[dict[str, object]] = []
            for path in sorted(Path(home).rglob("session.jsonl")):
                events.extend(
                    json.loads(line)
                    for line in path.read_text().splitlines()
                    if line.strip()
                )
            (output / "events.json").write_text(json.dumps(events, indent=2))
            user_messages = [
                event.get("data", {}).get("text")
                for event in events
                if event.get("kind") == "user/message"
            ]
            assert user_messages == ["PHASE2_FOCUS_COMPLETE"], user_messages
            titles = [
                event.get("data", {}).get("title")
                for event in events
                if event.get("kind") == "session/title"
            ]
            assert titles == ["Phase 2 Focus Session"], titles
            assertions.append("slash commands never entered durable model input")

            settings_evidence = {}
            for path in sorted(Path(home).glob("*")):
                if path.is_file() and path.stat().st_size <= 128 * 1024:
                    settings_evidence[path.name] = path.read_text(errors="replace")
            (output / "settings-evidence.json").write_text(
                json.dumps(settings_evidence, indent=2)
            )
            assert any(
                "focus_view" in content and "true" in content.lower()
                for content in settings_evidence.values()
            ), settings_evidence

    result: dict[str, object] = {
        "passed": True,
        "theme": theme,
        "screens": captures,
        "assertions": assertions,
        "prompt_colors": prompt_colors,
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "inference": "built-in fake provider",
        "external_provider": False,
        "workspace": "disposable temporary directory",
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=45)
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    run(
        args.binary,
        args.output.resolve(),
        theme=args.theme,
        columns=args.columns,
        rows=args.rows,
        timeout=args.timeout,
    )
