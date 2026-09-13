#!/usr/bin/env python3
"""Capture the real heycode help/catalog UI without sending an inference request."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import struct
import tempfile
import termios
import time
from pathlib import Path

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
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
    color: bool,
    seed_skill: bool,
) -> dict[str, object]:
    binary = binary.resolve()
    if not binary.is_file():
        raise FileNotFoundError(binary)
    output.mkdir(parents=True, exist_ok=True)
    captures: list[str] = []

    with tempfile.TemporaryDirectory(prefix="heycode-help-theme-") as folder:
        root = Path(folder)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        if seed_skill:
            skill = work / ".heycode" / "skills" / "audit-terminal"
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text(
                "---\n"
                "name: audit-terminal\n"
                "description: Inspect terminal help without model inference\n"
                "disable-model-invocation: true\n"
                "---\n\n"
                "Use only for this disposable help-panel audit.\n"
            )
        (home / "settings.toml").write_text(
            "schema_version = 1\n"
            "[settings.ui-preferences]\n"
            f'theme = "{theme}"\n'
        )
        tui = FullScreenTui(
            str(home),
            str(work),
            str(binary),
            fake=True,
            color=color,
            rows=42,
            columns=110,
        )
        screen = Screen(110, 42)
        stream = TerminalByteStream(screen)

        def read(seconds: float = 0.15) -> str:
            stream.feed(tui.read(seconds))
            return "\n".join(screen.display)

        def wait_for(*needles: str, timeout: float = 30) -> str:
            deadline = time.monotonic() + timeout
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

        def send(data: bytes) -> None:
            os.write(tui.fd, data)
            read()

        def capture(name: str) -> str:
            visible = read(0.35)
            (output / f"{name}.txt").write_text(visible)
            render_screen(
                screen,
                output / f"{name}.png",
                background="#f8f9fb" if theme == "heycode-light" else "#101014",
                foreground="#20242c" if theme == "heycode-light" else "#e8eaf0",
            )
            captures.append(name)
            return visible

        def click_help_tab(label: str) -> None:
            row, line = next(
                (row, line)
                for row, line in enumerate(screen.display)
                if "Help" in line
                and "General" in line
                and "Commands" in line
                and "Custom commands" in line
            )
            column = line.index(label) + 1
            send(
                (
                    f"\x1b[<0;{column + 1};{row + 1}M"
                    f"\x1b[<0;{column + 1};{row + 1}m"
                ).encode()
            )

        try:
            wait_for("shift+tab to cycle")
            send(b"/help\r")
            wait_for("Shortcuts", "current bindings")
            general = capture("01-general-wide")
            assert "heycode works with your code" in general

            click_help_tab("Commands")
            commands = wait_for("Search commands:", "unavailable commands include a reason")
            capture("02-commands-wide")
            count = re.search(r"(\d+) of (\d+) commands", commands)
            assert count is not None and count.group(1) == count.group(2), commands
            catalog_count = int(count.group(2))

            send(b"reset")
            alias = wait_for("/clear, /reset", "Create a new durable session")
            capture("03-alias-search-wide")
            assert "/new" in alias

            send(b"\x7f" * len("reset"))
            wait_for("85 of 85 commands")
            click_help_tab("Custom commands")
            custom_wide = (
                wait_for(
                    "Search custom commands:",
                    "/skill audit-terminal",
                    "source:",
                )
                if seed_skill
                else wait_for(
                    "0 of 0 installed custom commands and skills",
                    "No installed custom commands or skills were discovered",
                )
            )
            capture("04-custom-wide")
            if seed_skill:
                assert not any(
                    line.strip().startswith("/audit-terminal")
                    for line in custom_wide.splitlines()
                )
            click_help_tab("Commands")
            wait_for("Search commands:")
            send(b"reset")
            wait_for("/clear, /reset")

            screen.resize(lines=24, columns=45)
            fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 45, 0, 0))
            narrow_alias = wait_for("/clear, /reset")
            capture("05-alias-search-narrow")
            assert "Esc: close" in narrow_alias

            send(b"\x7f" * len("reset"))
            wait_for("85 of 85 commands")
            click_help_tab("Custom commands")
            custom = (
                wait_for(
                    "Search custom commands:",
                    "/skill audit-terminal",
                    "source:",
                )
                if seed_skill
                else wait_for(
                    "0 of 0 installed custom commands", "skills", "No installed custom commands"
                )
            )
            capture("06-custom-narrow")
            if seed_skill:
                assert not any(
                    line.strip().startswith("/audit-terminal")
                    for line in custom.splitlines()
                )
            send(b"audit")
            if seed_skill:
                wait_for("1 of 1 installed custom commands", "skills")
            else:
                wait_for("0 of 0 installed custom commands", "skills")
            capture("07-custom-search-narrow")

            send(b"\t")
            narrow_general = wait_for("Shortcuts", "Tab: tabs")
            capture("08-general-narrow")
            assert "current bindings" in narrow_general
            send(b"\x1b[F")
            wait_for("indings to customize")
            capture("09-general-narrow-scrolled")

            screen.resize(lines=42, columns=110)
            fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 42, 110, 0, 0))
            wait_for("current bindings")
            send(b"\t")
            wait_for("Search commands:")
            send(b"\x7f" * len("audit") + b"background")
            unavailable = wait_for("unavailable", "hosting requires")
            capture("10-unavailable-wide")
            assert "/background" in unavailable

            send(b"\x1b")
            paths = list(home.rglob("session.jsonl"))
            assert len(paths) == 1, paths
            events = [
                json.loads(line)
                for line in paths[0].read_text().splitlines()
                if line.strip()
            ]
            assert not any(
                event.get("kind") in ("user/message", "request/header")
                for event in events
            )
            (output / "events.json").write_text(json.dumps(events, indent=2))

            if not color:
                parameters = {
                    parameter
                    for match in re.finditer(rb"\x1b\[([0-9;]*)m", bytes(tui.transcript))
                    for parameter in match.group(1).split(b";")
                    if parameter
                }
                color_parameters = {
                    parameter
                    for parameter in parameters
                    if parameter in {b"38", b"48"}
                    or 30 <= int(parameter) <= 37
                    or 40 <= int(parameter) <= 47
                    or 90 <= int(parameter) <= 97
                    or 100 <= int(parameter) <= 107
                }
                assert not color_parameters, sorted(color_parameters)

            result: dict[str, object] = {
                "passed": True,
                "theme": theme,
                "color": color,
                "screens": captures,
                "catalog_count": catalog_count,
                "assertions": [
                    "general, commands, and custom commands tabs render from real registries",
                    (
                        "custom rows use the supported /skill name invocation and source attribution"
                        if seed_skill
                        else "empty custom registry renders an honest empty state"
                    ),
                    "mouse and keyboard tab changes cycle all three read-only tabs",
                    "canonical command help includes aliases and live unavailable reasons",
                    "wide and 45x24 narrow states remain usable and scroll to all content",
                    "help creates no durable user message or inference request",
                ]
                + (["NO_COLOR emitted no color SGR"] if not color else []),
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "source_reference": [
                    "tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/04-help.png",
                    "tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/04b-help-commands.png",
                    "tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/04c-help-custom.png",
                ],
                "inference_requests": 0,
                "seeded_skill": seed_skill,
                "workspace": "disposable temporary directory",
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result, indent=2))
            return result
        finally:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), required=True)
    parser.add_argument("--color", action="store_true")
    parser.add_argument("--empty-custom", action="store_true")
    arguments = parser.parse_args()
    run(
        arguments.binary,
        arguments.output,
        theme=arguments.theme,
        color=arguments.color,
        seed_skill=not arguments.empty_custom,
    )
