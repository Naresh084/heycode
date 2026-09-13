#!/usr/bin/env python3
"""Exercise local command controls through the real CLI TUI and a PTY.

The journey uses only heycode's repeating offline fake provider. One seed turn
creates durable, deliberately unreported usage; every command assertion after
that must leave the provider request count unchanged.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import tempfile
import time

import pyte

from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class Journey:
    def __init__(self, binary: Path, output: Path, home: Path, workspace: Path):
        self.binary = binary.resolve()
        self.output = output
        self.home = home
        self.workspace = workspace
        self.screen = Screen(112, 44)
        self.stream = pyte.ByteStream(self.screen)
        self.tui: FullScreenTui | None = None
        self.captures: list[str] = []
        self.transcript = bytearray()

    def start(self, *, resume: bool = False) -> None:
        self.screen.reset()
        self.tui = FullScreenTui(
            str(self.home),
            str(self.workspace),
            str(self.binary),
            fake=True,
            color=True,
            rows=44,
            columns=112,
            extra=["--continue"] if resume else [],
        )
        self.wait("heycode 0.1.0")

    def read(self, seconds: float = 0.15) -> None:
        assert self.tui is not None
        self.stream.feed(self.tui.read(seconds))

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def wait(self, needle: str, timeout: float = 45) -> str:
        assert self.tui is not None
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.read()
            visible = self.text()
            if needle in visible:
                return visible
            if not self.tui.alive():
                raise AssertionError(
                    f"CLI exited while waiting for {needle!r}:\n{visible}"
                )
        raise AssertionError(f"Timed out waiting for {needle!r}:\n{self.text()}")

    def send(self, value: bytes) -> None:
        assert self.tui is not None
        os.write(self.tui.fd, value)
        self.read()

    def command(self, value: str, expected: str) -> str:
        self.send(value.encode())
        self.send(b"\r")
        return self.wait(expected)

    def capture(self, name: str) -> None:
        self.read(0.3)
        (self.output / f"{name}.txt").write_text(self.text())
        render_screen(self.screen, self.output / f"{name}.png")
        self.captures.append(name)

    def stop(self) -> None:
        if self.tui is None:
            return
        tui = self.tui
        if tui.alive():
            self.send(b"/quit\r")
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and tui.alive():
                self.read(0.1)
        self.transcript.extend(tui.transcript)
        tui.close()
        self.tui = None

    def journal_events(self) -> list[dict]:
        paths = sorted(self.home.rglob("session.jsonl"))
        assert len(paths) == 1, paths
        return [
            json.loads(line)
            for line in paths[0].read_text().splitlines()
            if line.strip()
        ]


def inference_turn_count(events: list[dict]) -> int:
    return sum(event.get("kind") == "turn/start" for event in events)


def run(binary: Path, output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="heycode-command-controls-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        journey = Journey(binary, output, home, workspace)
        try:
            journey.start()
            journey.command("CONTROL_ACCEPTANCE_SEED", "FAKE-REPLY: offline smoke response.")
            seeded = journey.journal_events()
            assert inference_turn_count(seeded) == 1, seeded

            journey.command(
                "/rename Command   controls acceptance",
                "Renamed to Command   controls acceptance",
            )
            journey.capture("01-rename-spaces")

            journey.command("/scroll-speed", "Scroll speed")
            initial_ruler = next(
                line for line in journey.text().splitlines() if "ruler" in line
            )
            # SGR button 64 is mouse-wheel up. The picker consumes it as a
            # preview and must not scroll the transcript behind the modal.
            journey.send(b"\x1b[<64;55;20M")
            journey.read(0.3)
            moved_ruler = next(
                line for line in journey.text().splitlines() if "ruler" in line
            )
            assert moved_ruler != initial_ruler, (initial_ruler, moved_ruler)
            journey.send(b"\x1b[C")
            journey.wait("1.25x")
            journey.capture("02-scroll-keyboard-mouse-preview")
            journey.send(b"\r")
            journey.wait("scroll speed persisted as 1.25x")
            journey.command("/scroll-speed unexpected", "usage: /scroll-speed")
            journey.capture("03-scroll-persisted-and-error")

            journey.command("/statusline hints off", "status line: on; command hints: off")
            journey.command("/statusline", "status line: on; command hints: off")
            journey.command(
                "/statusline unexpected",
                "usage: /statusline [on|off|hints on|hints off|reset]",
            )
            journey.capture("04-statusline-state-and-error")

            journey.command("/autocompact 250k", "250,000 input tokens; saved and applied")
            journey.command("/autocompact", "250,000 input tokens")
            journey.command("/autocompact 42", "usage: /autocompact [auto|100k-1m]")
            journey.capture("05-autocompact-state-and-error")

            turns_before_insights = inference_turn_count(journey.journal_events())
            insights = journey.command("/insights", "durable friction signals:")
            for required in [
                "scope: all 1 used sessions",
                "privacy: report aggregates structural journal fields only",
                "usage: unknown",
                "counting: physical local journal suffixes only",
            ]:
                assert required in insights, (required, insights)
            journey.capture("06-insights-local-unknown")
            journey.command("/insights unexpected", "usage: /insights")
            assert inference_turn_count(journey.journal_events()) == turns_before_insights
            journey.capture("07-insights-error-no-inference")

            journey.stop()
            journey.start(resume=True)
            journey.command("/statusline", "status line: on; command hints: off")
            journey.command("/autocompact", "250,000 input tokens")
            journey.command("/scroll-speed", "Scroll speed")
            journey.wait("1.25x")
            journey.capture("08-restart-persistence")
            journey.send(b"\x1b")
            journey.wait("Command   controls acceptance")
            assert inference_turn_count(journey.journal_events()) == turns_before_insights

            events = journey.journal_events()
            journey.stop()
            (output / "terminal.ansi").write_bytes(journey.transcript)
            (output / "events.json").write_text(json.dumps(events, indent=2))
            (output / "result.json").write_text(
                json.dumps(
                    {
                        "status": "passed",
                        "runtime": "real heycode CLI TUI over PTY",
                        "provider": "offline repeating fake",
                        "durable_inference_turns": inference_turn_count(events),
                        "inference_turns_from_commands": 0,
                        "captures": journey.captures,
                        "restart_persistence": {
                            "scroll_speed": "1.25x",
                            "statusline": "on",
                            "command_hints": "off",
                            "autocompact": 250000,
                            "title": "Command   controls acceptance",
                        },
                    },
                    indent=2,
                )
            )
        finally:
            journey.stop()
            if journey.transcript:
                (output / "terminal.ansi").write_bytes(journey.transcript)
    print(f"PASS: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    run(args.binary, args.output)
