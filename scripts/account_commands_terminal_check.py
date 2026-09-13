#!/usr/bin/env python3
"""Capture heycode `/logout`, `/login` and `/connect` surfaces with zero inference.

The process uses a disposable HEYCODE_HOME and workspace with the built-in fake
provider, so no credential store, provider endpoint or model request is
touched. `/connect` is opened only far enough to show its provider picker and
is then cancelled with Escape; no secret is ever entered.
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


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, out: Path, *, color: bool, columns: int, rows: int) -> dict:
    binary = binary.resolve(strict=True)
    out.mkdir(parents=True, exist_ok=False)
    captures: list[str] = []
    screen = Screen(columns, rows)
    stream = TerminalByteStream(screen)
    palette = (
        {"foreground": "#dddddd", "background": "#101014"}
        if color
        else {"foreground": "#dddddd", "background": "#101014"}
    )
    result: dict = {"status": "failed", "captures": captures, "provider_requests": 0}
    with tempfile.TemporaryDirectory(prefix="heycode-account-home-") as home, tempfile.TemporaryDirectory(
        prefix="heycode-account-workspace-"
    ) as workspace:
        tui = FullScreenTui(home, workspace, str(binary), fake=True, color=color, rows=rows, columns=columns)

        def read(seconds: float) -> str:
            try:
                stream.feed(tui.read(seconds))
            except OSError:
                pass  # the process may have exited after /logout
            return "\n".join(screen.display)

        def send(data: bytes) -> None:
            os.write(tui.fd, data)

        def capture(name: str) -> str:
            text = read(0.35)
            (out / f"{name}.txt").write_text(text)
            render_screen(screen, out / f"{name}.png", **palette)
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
            send(b"/login"); read(0.6); capture("01-login-command-menu")
            send(b"\r"); read(2.0); capture("02-login-picker")
            send(b"\x1b"); read(1.0); capture("03-login-cancelled")
            send(b"/connect"); read(0.6); capture("04-connect-command-menu")
            send(b"\r"); read(2.0); capture("05-connect-picker")
            send(b"\x1b"); read(1.0); capture("06-connect-cancelled")
            # `/logout` prints its receipt and exits the process like the
            # source, so it runs last and the terminal may close underneath.
            send(b"/logout"); read(0.6); capture("07-logout-command-menu")
            send(b"\r")
            try:
                read(2.0)
            except OSError:
                pass
            capture("08-logout-result")
            result = {
                "status": "captured",
                "engine": "heycode",
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "viewport": [columns, rows],
                "color": color,
                "captures": captures,
                "provider": "fake",
                "model_prompt_sent": False,
                "secret_entered": False,
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
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    args = parser.parse_args()
    run(args.binary, args.output, color=not args.no_color, columns=args.columns, rows=args.rows)
