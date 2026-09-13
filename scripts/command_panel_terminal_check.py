#!/usr/bin/env python3
"""Capture one heycode slash-command panel with the fake provider and zero inference.

Each invocation starts a fresh heycode process in a disposable HEYCODE_HOME and
workspace, types the command, captures the completion menu, presses Enter,
captures the opened surface, presses the requested dismissal keys and captures
the result. Only dialog-style commands belong here.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
import time
from pathlib import Path

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui

KEYS = {"escape": b"\x1b", "enter": b"\r", "down": b"\x1b[B", "up": b"\x1b[A", "tab": b"\t", "space": b" ", "backspace": b"\x7f"}


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, out: Path, command: str, keys: list[str], *, color: bool, columns: int, rows: int, settle: float, theme: str = "heycode-dark") -> dict:
    binary = binary.resolve(strict=True)
    out.mkdir(parents=True, exist_ok=False)
    captures: list[str] = []
    screen = Screen(columns, rows)
    stream = TerminalByteStream(screen)
    result: dict = {"status": "failed", "captures": captures}
    with tempfile.TemporaryDirectory(prefix="heycode-panel-home-") as home, tempfile.TemporaryDirectory(prefix="heycode-panel-workspace-") as workspace:
        (Path(workspace) / "README.md").write_text("# Isolated panel check workspace\n")
        (Path(home) / "settings.toml").write_text(
            "schema_version = 1\n[settings.ui-preferences]\n" f'theme = "{theme}"\n'
        )
        tui = FullScreenTui(home, workspace, str(binary), fake=True, color=color, rows=rows, columns=columns)

        def read(seconds: float) -> str:
            stream.feed(tui.read(seconds))
            return "\n".join(screen.display)

        def capture(name: str) -> str:
            text = read(0.35)
            (out / f"{name}.txt").write_text(text)
            render_screen(
                screen,
                out / f"{name}.png",
                background="#f8f9fb" if theme == "heycode-light" else "#101014",
                foreground="#20242c" if theme == "heycode-light" else "#dddddd",
            )
            captures.append(name)
            return text

        try:
            deadline = time.time() + 30
            while b"\x1b[?2004h" not in tui.transcript and time.time() < deadline:
                tui.read(0.2)
                if not tui.alive():
                    raise RuntimeError("heycode exited before drawing the composer")
            # Everything painted before the composer became ready was only
            # recorded in the transcript; replay it so the emulator starts
            # from the complete first frame.
            stream.feed(bytes(tui.transcript))
            read(1.0)
            capture("00-start")
            os.write(tui.fd, command.encode()); read(0.6); capture("01-command-menu")
            os.write(tui.fd, b"\r"); read(settle); capture("02-opened")
            for index, key in enumerate(keys, start=3):
                os.write(tui.fd, KEYS[key]); read(1.0); capture(f"{index:02d}-after-{key}")
            result = {
                "status": "captured",
                "engine": "heycode",
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "viewport": [columns, rows],
                "color": color,
                "theme": theme,
                "command": command,
                "keys": keys,
                "captures": captures,
                "provider": "fake",
                "model_prompt_sent": False,
                "process_alive_at_end": tui.alive(),
            }
        except Exception as error:  # noqa: BLE001 - recorded in the receipt
            result.update(status="failed", error=str(error))
        finally:
            (out / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    if result["status"] != "captured":
        raise SystemExit(1)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--command", required=True)
    parser.add_argument("--keys", nargs="*", default=["escape"], choices=sorted(KEYS))
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    parser.add_argument("--settle", type=float, default=2.0)
    args = parser.parse_args()
    run(args.binary, args.output, args.command, args.keys, color=not args.no_color, columns=args.columns, rows=args.rows, settle=args.settle, theme=args.theme)
