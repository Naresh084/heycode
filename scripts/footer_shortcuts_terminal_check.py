#!/usr/bin/env python3
"""Capture the heycode idle footer and the standalone `?` shortcut list.

The journey mirrors the pinned Claude Code footer reference state for state:
idle footer, `?` on an empty composer, Backspace, a question mark inside an
ordinary draft, and Escape. It runs the real binary on a pseudo-terminal with
the built-in fake adapter, a disposable HEYCODE_HOME and workspace, and a
loopback recorder standing in for the provider so the absence of inference is
observed rather than asserted from intent.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time

from agent_navigation_terminal_check import Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui

# The footer phrasing this build renders for each effective permission ID.
FOOTER_LABELS = {
    "ask": "⏸ manual mode on",
    "plan": "⏸ plan mode on",
    "accepted_edits": "⏵⏵ accept edits on",
    "auto": "⏵⏵ auto mode on",
    "full_access": "⏵⏵ full access on",
    "deny": "⏸ tools blocked",
}

# Entries the list must offer, and wording it must never offer because this
# build has no such binding.
REQUIRED_ENTRIES = [
    "/ for commands",
    "/mention for files",
    "/btw for side question",
    "shift + tab to cycle permissions",
    "ctrl + t to toggle tasks",
    "backslash (\\) + return (⏎) for",
    "/keybindings to customize",
]
FORBIDDEN_ENTRIES = ["for shell mode", "to suspend", "$EDITOR", "stash prompt"]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(
    binary: Path,
    output: Path,
    *,
    theme: str,
    color: bool,
    approval: str,
    columns: int,
    rows: int,
) -> dict[str, object]:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    requests: list[str] = []

    class Recorder(http.server.BaseHTTPRequestHandler):
        def refuse(self) -> None:
            requests.append(f"{self.command} {self.path}")
            self.send_response(500)
            self.send_header("Content-Length", "0")
            self.end_headers()

        do_GET = do_POST = refuse

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Recorder)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    result: dict[str, object] = {
        "status": "failed",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "viewport": [columns, rows],
        "theme": theme,
        "color": color,
        "permission_mode": approval,
        "captures": [],
        "assertions": {},
        "provider_requests": requests,
    }
    tui = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-footer-shortcuts-") as folder:
            root = Path(folder)
            home, work = root / "home", root / "work"
            home.mkdir()
            work.mkdir()
            (home / "settings.toml").write_text(
                "schema_version = 1\n"
                "[settings.ui-preferences]\n"
                f'theme = "{theme}"\n'
            )
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            tui = FullScreenTui(
                str(home),
                str(work),
                str(binary),
                fake=True,
                color=color,
                rows=rows,
                columns=columns,
                extra=[
                    "--set",
                    f"approval.mode={'default' if approval == 'plan' else approval}",
                    "--set",
                    f"llm.base_url={base}",
                ],
            )
            screen = Screen(columns, rows)
            stream = TerminalByteStream(screen)

            def read(seconds: float = 0.15) -> str:
                stream.feed(tui.read(seconds))
                return "\n".join(screen.display)

            def wait_for(*needles: str, timeout: float = 30) -> str:
                deadline = time.monotonic() + timeout
                latest = read()
                while time.monotonic() < deadline:
                    if all(needle in latest for needle in needles):
                        return latest
                    if not tui.alive():
                        raise AssertionError(f"heycode exited waiting for {needles}:\n{latest}")
                    latest = read()
                raise AssertionError(f"Missing {needles}:\n{latest}")

            def send(data: bytes, seconds: float = 0.4) -> str:
                os.write(tui.fd, data)
                return read(seconds)

            def capture(name: str) -> str:
                visible = read(0.35)
                (output / f"{name}.txt").write_text(visible)
                render_screen(
                    screen,
                    output / f"{name}.png",
                    background="#f8f9fb" if theme == "heycode-light" else "#101014",
                    foreground="#20242c" if theme == "heycode-light" else "#dddddd",
                )
                result["captures"].append(name)
                return visible

            def footer(text: str) -> str:
                return next(
                    (line for line in reversed(text.split("\n")) if line.strip()),
                    "",
                )

            label = FOOTER_LABELS[approval]
            if approval == "plan":
                # Plan is entered through its session-mode owner, not through
                # `approval.mode`, so the journey asks for it the way a user
                # would and waits for the committed footer.
                wait_for(FOOTER_LABELS["ask"])
                os.write(tui.fd, b"/permissions plan\r")
            start = wait_for(label)
            start = capture("00-start")
            assertions = result["assertions"]
            assertions["idle_footer_is_one_line_below_the_composer"] = footer(
                start
            ).startswith(f"  {label}")
            assertions["idle_footer_omits_model_context_and_token_row"] = not any(
                marker in start for marker in ("ctx:", "in:", "out:")
            )
            if approval == "ask":
                assertions["default_mode_offers_the_shortcut_list"] = (
                    "? for shortcuts" in footer(start)
                )
            else:
                assertions["other_modes_offer_the_cycle_chord"] = (
                    "(shift+tab to cycle)" in footer(start)
                )

            opened = send(b"?", 0.6)
            opened = capture("01-shortcuts-open")
            visible_rows = opened.split("\n")
            assertions["question_mark_is_not_inserted_into_the_draft"] = not any(
                line.startswith("❯ ?") or line.startswith("> ?") for line in visible_rows
            )
            assertions["shortcut_list_replaces_the_footer_line"] = label not in opened
            # Wrapping breaks entries across rows on a narrow terminal, so
            # the wording assertion runs where a row can hold one entry whole.
            flat = " ".join(line.strip() for line in visible_rows)
            assertions["shortcut_list_lists_only_supported_bindings"] = (
                columns < 90
                or all(entry in opened for entry in REQUIRED_ENTRIES)
            ) and not any(entry in flat for entry in FORBIDDEN_ENTRIES)
            assertions["shortcut_list_stays_inside_the_viewport"] = all(
                len(line) <= columns for line in visible_rows
            )

            cleared = send(b"\x7f", 0.5)
            cleared = capture("02-shortcuts-cleared")
            assertions["backspace_restores_the_footer"] = footer(cleared).startswith(
                f"  {label}"
            ) and "/keybindings to customize" not in cleared

            typed = send(b"hello?", 0.5)
            typed = capture("03-question-in-draft")
            assertions["question_mark_in_a_draft_is_ordinary_text"] = (
                "hello?" in typed and "/keybindings to customize" not in typed
            )
            assertions["draft_footer_drops_the_discovery_hint"] = (
                "? for shortcuts" not in footer(typed)
            )

            send(b"\x15", 0.3)
            send(b"?", 0.4)
            escaped = send(b"\x1b", 0.4)
            escaped = capture("04-shortcuts-escape")
            assertions["escape_restores_the_footer"] = footer(escaped).startswith(
                f"  {label}"
            ) and "/keybindings to customize" not in escaped

            assertions["zero_provider_requests"] = not requests
            result["status"] = "passed" if all(assertions.values()) else "failed"
    except Exception as error:  # noqa: BLE001 - recorded, then re-reported
        result["failure"] = f"{type(error).__name__}: {error}"
    finally:
        if tui is not None:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", default="heycode-dark")
    parser.add_argument("--no-color", dest="color", action="store_false")
    parser.add_argument("--approval", default="ask", choices=sorted(FOOTER_LABELS))
    parser.add_argument("--viewport", default="110x42", help="COLUMNSxROWS terminal size")
    arguments = parser.parse_args()
    width, _, height = arguments.viewport.partition("x")
    outcome = run(
        arguments.binary,
        arguments.output,
        theme=arguments.theme,
        color=arguments.color,
        approval=arguments.approval,
        columns=int(width),
        rows=int(height),
    )
    print(json.dumps(outcome, indent=2))
    raise SystemExit(outcome["status"] != "passed")
