#!/usr/bin/env python3
"""Verify the real CLI help panel without a provider request."""
from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import struct
import tempfile
import termios
import time

import pyte
from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


def run(binary: Path, output: Path):
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="heycode-help-panel-") as folder:
        root = Path(folder)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        tui = FullScreenTui(str(home), str(work), str(binary.resolve()), fake=True,
                            color=True, rows=42, columns=110)
        screen = pyte.Screen(110, 42)
        stream = pyte.ByteStream(screen)
        captures = []

        def read():
            stream.feed(tui.read(.15))

        def visible():
            return "\n".join(screen.display)

        def wait(needle):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                read()
                if needle in visible():
                    return
                if not tui.alive():
                    raise AssertionError(f"CLI exited while waiting for {needle}: {visible()}")
            raise AssertionError(f"Missing {needle}: {visible()}")

        def send(data):
            os.write(tui.fd, data)
            read()

        def wait_closed():
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                read()
                if not any("Help" in line and "General" in line and "Commands" in line
                           for line in screen.display):
                    return
            raise AssertionError(f"Help did not close: {visible()}")

        def capture(name):
            read()
            (output / f"{name}.txt").write_text(visible())
            render_screen(screen, output / f"{name}.png")
            captures.append(name)

        def click_tab(label):
            row, line = next((row, line) for row, line in enumerate(screen.display)
                             if "Help" in line and "General" in line and "Commands" in line)
            column = line.index(label) + 1
            send(f"\x1b[<0;{column + 1};{row + 1}M\x1b[<0;{column + 1};{row + 1}m".encode())

        try:
            wait("shift+tab to cycle")
            send(b"/help\r")
            wait("current bindings")
            capture("01-general")
            click_tab("Commands")
            wait("Search commands:")
            capture("02-commands-mouse")
            send(b"reset")
            wait("/clear, /reset")
            capture("03-alias-search")
            send(b"\x7f" * 5 + b"background")
            wait("unavailable")
            wait("hosting requires")
            capture("04-unavailable-reason")
            send(b"\x7f" * 10 + b"does-not-exist")
            wait("No matching commands")
            capture("05-empty-search")
            screen.resize(lines=24, columns=45)
            fcntl.ioctl(tui.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 45, 0, 0))
            wait("No matching commands")
            capture("06-narrow")
            # Bracketed paste inside the read-only modal must not leak into the
            # composer or become an accidental model prompt after closing.
            send(b"\x1b[200~do-not-submit-this\x1b[201~")
            send(b"\x1b")
            wait_closed()
            assert "do-not-submit-this" not in visible()
            send(b"/help\r")
            wait("current bindings")
            send(b"\t")
            wait("Search commands:")
            capture("07-keyboard-tab-reopen")
            send(b"\x1b")
            paths = list(home.rglob("session.jsonl"))
            assert len(paths) == 1, paths
            events = [json.loads(line) for line in paths[0].read_text().splitlines()]
            assert not any(event.get("kind") in ("user/message", "request/header") for event in events)
            (output / "events.json").write_text(json.dumps(events, indent=2))
            (output / "result.json").write_text(json.dumps({
                "status": "passed", "captures": captures, "model_requests": 0,
                "mouse_tabs": True, "keyboard_tabs": True, "alias_search": True,
                "unavailable_reason": True, "narrow_resize": True,
                "paste_does_not_submit": True,
            }, indent=2))
        finally:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
    print(f"PASS: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    run(arguments.binary, arguments.output)
