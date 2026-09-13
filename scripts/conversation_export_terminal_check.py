#!/usr/bin/env python3
"""Verify current-conversation export through the production binary and a PTY.

The one model turn is served by a loopback OpenRouter-shaped SSE fixture.  The
export branches then resume independent copies of that durable session so the
file and clipboard byte streams can be compared from exactly the same state.
No paid provider, real credential, host clipboard, or user workspace is used.
"""
from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import struct
import tempfile
import termios
import threading
import time

import pyte

from plan_review_pty import MODEL
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui, plain, shows


ANSWER = "EXPORT-FIXTURE-ANSWER"
PROMPT = "Seed the export fixture"
LARGE_PROMPT = "Seed the LARGE export fixture"
LARGE_ANSWER = "LARGE-BEGIN\n" + ("0123456789abcdef\n" * 4_200) + "LARGE-END"
OSC52 = re.compile(rb"\x1b\]52;c;([A-Za-z0-9+/=]*)\x07")


class Screen(pyte.Screen):
    """Reset stale cells when the application re-enters its alternate screen."""

    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            payload = (
                {"data": {"label": "export fixture"}}
                if self.path.endswith("/key")
                else (
                    {"data": MODEL}
                    if "/model/" in self.path
                    else {"data": [MODEL], "total_count": 1, "links": {"next": None}}
                )
            )
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            serialized = json.dumps(request)
            answer = LARGE_ANSWER if LARGE_PROMPT in serialized else ANSWER
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for delta, finish in (({"content": answer}, None), ({}, "stop")):
                chunk = {
                    "id": "export-fixture",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {"index": 0, "delta": delta, "finish_reason": finish}
                    ],
                }
                self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                self.wfile.flush()
            usage = {
                "id": "export-fixture",
                "choices": [],
                "usage": {
                    "prompt_tokens": 11,
                    "completion_tokens": 3,
                    "total_tokens": 14,
                },
            }
            self.wfile.write(("data: " + json.dumps(usage) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    previous_ssh = os.environ.get("SSH_CONNECTION")
    # Force the terminal-owned OSC 52 path. This deliberately avoids pbcopy.
    os.environ["SSH_CONNECTION"] = "export-fixture"
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-export-pty-") as temporary:
            root = Path(temporary)
            seed_home, workspace = root / "seed-home", root / "workspace"
            seed_home.mkdir()
            workspace.mkdir()
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (seed_home / "settings.toml").write_text("schema_version = 1\n")
            (seed_home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_EXPORT_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            os.environ["HEYCODE_EXPORT_FIXTURE"] = "loopback-only"
            extra = [
                "--provider",
                "openrouter",
                "--model",
                MODEL["id"],
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_EXPORT_FIXTURE",
            ]

            def launch(
                home: Path, resume: Path | None = None, *, color: bool = True
            ):
                args = extra + (["--resume", str(resume)] if resume else [])
                tui = FullScreenTui(
                    str(home),
                    str(workspace),
                    str(binary),
                    fake=False,
                    color=color,
                    rows=42,
                    columns=110,
                    extra=args,
                )
                screen = Screen(110, 42)
                return tui, screen, TerminalByteStream(screen)

            def drive(tui, screen, stream, *, light: bool = False):
                size = {"rows": screen.lines, "columns": screen.columns}

                def read(seconds: float = 0.15):
                    stream.feed(tui.read(seconds))

                def visible() -> str:
                    return "\n".join(screen.display)

                def wait(needle: str, timeout: float = 30) -> None:
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        read()
                        if needle in visible():
                            return
                        if not tui.alive():
                            raise AssertionError(
                                f"CLI exited while waiting for {needle}: {visible()}"
                            )
                    raise AssertionError(f"Missing {needle}: {visible()}")

                def wait_new(needle: str, offset: int, timeout: float = 30) -> None:
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        read()
                        delta = plain(tui.transcript[offset:])
                        if shows(delta, needle):
                            return
                        if not tui.alive():
                            raise AssertionError(
                                f"CLI exited while waiting for new {needle}: {delta[-2000:]}"
                            )
                    raise AssertionError(
                        f"Missing new {needle}: {plain(tui.transcript[offset:])[-3000:]}"
                    )

                def wait_path(path: Path, timeout: float = 30) -> None:
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        read()
                        if path.is_file():
                            return
                        if not tui.alive():
                            break
                    raise AssertionError(f"Export target was not created: {path}")

                def send(data: bytes) -> None:
                    os.write(tui.fd, data)
                    read()

                def resize(rows: int, columns: int) -> str:
                    size.update(rows=rows, columns=columns)
                    screen.resize(lines=rows, columns=columns)
                    fcntl.ioctl(
                        tui.fd,
                        termios.TIOCSWINSZ,
                        struct.pack("HHHH", rows, columns, 0, 0),
                    )
                    read(0.5)
                    return visible()

                def repaint() -> str:
                    rows, columns = size["rows"], size["columns"]
                    screen.resize(lines=rows, columns=columns + 1)
                    fcntl.ioctl(
                        tui.fd,
                        termios.TIOCSWINSZ,
                        struct.pack("HHHH", rows, columns + 1, 0, 0),
                    )
                    read(0.5)
                    resize(rows, columns)
                    return visible()

                def capture(name: str) -> str:
                    value = repaint()
                    (output / f"{name}.txt").write_text(value)
                    render_screen(
                        screen,
                        output / f"{name}.png",
                        background="#f8f9fb" if light else "#101014",
                        foreground="#20242c" if light else "#e8eaf0",
                    )
                    return value

                return read, visible, wait, wait_new, wait_path, send, capture, resize

            # Create one real durable conversation via the loopback model.
            tui, screen, stream = launch(seed_home)
            read, _visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait("shift+tab to cycle")
                send((PROMPT + "\r").encode())
                wait(ANSWER)
                capture("01-seeded-conversation")
                logs = list(seed_home.rglob("session.jsonl"))
                assert len(logs) == 1, logs
                seed_log = logs[0]
                seed_events = [
                    json.loads(line) for line in seed_log.read_text().splitlines()
                ]
                assert len(requests) == 1, requests
                assert sum(event["kind"] == "user/message" for event in seed_events) == 1
            finally:
                tui.close()

            # Clone the settled durable state before any export command. This
            # makes the clipboard and file snapshots byte-for-byte comparable.
            file_home, clipboard_home = root / "file-home", root / "clipboard-home"
            light_home, no_color_home = root / "light-home", root / "no-color-home"
            for branch_home in (
                file_home,
                clipboard_home,
                light_home,
                no_color_home,
            ):
                shutil.copytree(seed_home, branch_home)
            seed_relative = seed_log.relative_to(seed_home)

            # File branch: source-matched modal transitions and safe writes.
            tui, screen, stream = launch(file_home, file_home / seed_relative)
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait(ANSWER)
                assert len(requests) == 1, "resume sent an unintended model request"

                send(b"/export fixture-export.txt\r")
                direct = workspace / "fixture-export.txt"
                wait_path(direct)
                exact_file = direct.read_bytes()
                assert ANSWER.encode() in exact_file and PROMPT.encode() in exact_file
                # The command that creates the snapshot must not export itself.
                assert b"/export fixture-export.txt" not in exact_file
                (output / "exact-file-export.txt").write_bytes(exact_file)
                capture("02-direct-file-success")

                send(b"/export\r")
                wait("Export conversation")
                chooser = capture("03-method-chooser")
                assert "Select export method" in chooser
                assert "Copy to clipboard" in chooser and "Save to file" in chooser

                send(b"\x1b[B\r")
                wait("Enter filename:")
                form = capture("04-generated-filename-form")
                assert "heycode-conversation.txt" in form, form
                send(b"\x1b")
                wait("Select export method")
                back = capture("05-form-escape-back")
                assert "Save to file" in back
                send(b"\x1b")
                wait("Export cancelled")
                capture("06-chooser-cancelled")

                # Create-only means retrying the same target is an atomic refusal.
                before = len(tui.transcript)
                send(b"/export fixture-export.txt\r")
                wait_new("destination already exists or changed", before)
                assert direct.read_bytes() == exact_file
                capture("07-no-clobber-refusal")

                extensionless = workspace / "nested/missing/export-copy.txt"
                send(b"/export file nested/missing/export-copy\r")
                wait_path(extensionless)
                capture("08-extensionless-parent-create")

                occupied = workspace / "occupied.txt"
                occupied.mkdir()
                before = len(tui.transcript)
                send(b"/export occupied.txt\r")
                wait_new("destination already exists or changed", before)
                assert occupied.is_dir()
                capture("09-directory-target-refusal")

                outside = root / "outside-export.txt"
                send(f"/export file {outside}\r".encode())
                wait("outside the allowed filesystem roots")
                assert not outside.exists()
                capture("10-outside-root-refusal")

                # Exercise the editable form's successful extensionless path.
                send(b"/export\r")
                wait("Select export method")
                send(b"\x1b[B\r")
                wait("Enter filename:")
                send(b"\x15")
                send(b"form-export\r")
                wait_path(workspace / "form-export.txt")
                capture("11-form-save-success")

                long_name = "long-" + ("x" * 180) + ".txt"
                send(f"/export {long_name}\r".encode())
                wait_path(workspace / long_name)
                capture("12-long-path-success")

                send(b"/export\r")
                wait("Select export method")
                save_row, save_line = next(
                    (index, line)
                    for index, line in enumerate(screen.display)
                    if "2. Save to file" in line
                )
                save_column = save_line.index("2. Save to file")
                click = (
                    f"\x1b[<0;{save_column + 1};{save_row + 1}M"
                    f"\x1b[<0;{save_column + 1};{save_row + 1}m"
                ).encode()
                send(click)
                selected = capture("13-mouse-save-selected")
                assert "› 2. Save to file" in selected, selected
                send(b"\r")
                wait("Enter filename:")
                capture("14-mouse-save-activated")
                send(b"\x1b")
                wait("Select export method")
                before = len(tui.transcript)
                send(b"\x1b")
                wait_new("Export cancelled", before)

                send(b"/export\r")
                wait("Select export method")
                resize(30, 52)
                wait("Export conversation")
                narrow = capture("15-narrow-chooser")
                assert "Copy to clipboard" in narrow and "Save to file" in narrow
                assert "Esc to cancel" in narrow
                send(b"\x1b")
                wait("Export cancelled")
                resize(42, 110)

                file_logs = list(file_home.rglob("session.jsonl"))
                assert len(file_logs) == 1, file_logs
                file_events = [
                    json.loads(line) for line in file_logs[0].read_text().splitlines()
                ]
                (output / "file-branch-events.json").write_text(
                    json.dumps(file_events, indent=2)
                )
            finally:
                (output / "file-branch.ansi").write_bytes(tui.transcript)
                tui.close()

            # Clipboard branch resumes the identical pre-export state. OSC 52
            # is intercepted from the PTY and never reaches the host clipboard.
            tui, screen, stream = launch(
                clipboard_home, clipboard_home / seed_relative
            )
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait(ANSWER)
                assert len(requests) == 1, "clipboard resume sent a model request"
                before = len(tui.transcript)
                send(b"/export\r")
                wait("Export conversation")
                send(b"\r")
                wait("Conversation copied to clipboard")
                capture("16-clipboard-success")
                matches = OSC52.findall(tui.transcript[before:])
                assert len(matches) == 1, matches
                clipboard = base64.b64decode(matches[0], validate=True)
                assert clipboard == exact_file, (
                    clipboard.decode(errors="replace"),
                    exact_file.decode(errors="replace"),
                )
                assert b"/export" not in clipboard
                (output / "exact-clipboard-export.txt").write_bytes(clipboard)
                clipboard_logs = list(clipboard_home.rglob("session.jsonl"))
                assert len(clipboard_logs) == 1, clipboard_logs
                clipboard_events = [
                    json.loads(line)
                    for line in clipboard_logs[0].read_text().splitlines()
                ]
                (output / "clipboard-branch-events.json").write_text(
                    json.dumps(clipboard_events, indent=2)
                )
            finally:
                (output / "clipboard-branch.ansi").write_bytes(tui.transcript)
                tui.close()

            # The current theme and color capability are supplied by the real
            # application startup path, not painted by the harness.
            (light_home / "settings.toml").write_text(
                "schema_version = 1\n"
                "[settings.ui-preferences]\n"
                'theme = "heycode-light"\n'
            )
            tui, screen, stream = launch(light_home, light_home / seed_relative)
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream, light=True
            )
            try:
                wait(ANSWER)
                assert len(requests) == 1, "light replay sent a model request"
                send(b"/export\r")
                wait("Select export method")
                capture("17-light-chooser")
                send(b"\x1b")
                wait("Export cancelled")
            finally:
                (output / "light-branch.ansi").write_bytes(tui.transcript)
                tui.close()

            tui, screen, stream = launch(
                no_color_home, no_color_home / seed_relative, color=False
            )
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait(ANSWER)
                assert len(requests) == 1, "NO_COLOR replay sent a model request"
                send(b"/export\r")
                wait("Select export method")
                capture("18-no-color-chooser")
                send(b"\x1b")
                wait("Export cancelled")
            finally:
                (output / "no-color-branch.ansi").write_bytes(tui.transcript)
                tui.close()
            parameters = {
                parameter
                for match in re.finditer(rb"\x1b\[([0-9;]*)m", tui.transcript)
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

            # A >64 KiB transcript proves file export remains exact across a
            # restart while the bounded terminal clipboard fails explicitly.
            large_home = root / "large-home"
            large_home.mkdir()
            shutil.copyfile(seed_home / "settings.toml", large_home / "settings.toml")
            shutil.copyfile(seed_home / "config.toml", large_home / "config.toml")
            tui, screen, stream = launch(large_home)
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait("shift+tab to cycle")
                send((LARGE_PROMPT + "\r").encode())
                wait("LARGE-END", timeout=45)
                assert len(requests) == 2, requests
                send(b"/export large-current.txt\r")
                wait_path(workspace / "large-current.txt", timeout=45)
                large_current = (workspace / "large-current.txt").read_bytes()
                assert len(large_current) > 64 * 1024
                assert b"LARGE-BEGIN" in large_current and b"LARGE-END" in large_current
                (output / "large-current-export.txt").write_bytes(large_current)
                capture("19-large-file-current")
                large_logs = list(large_home.rglob("session.jsonl"))
                assert len(large_logs) == 1, large_logs
                large_log = large_logs[0]
                large_events = [
                    json.loads(line) for line in large_log.read_text().splitlines()
                ]
                assert sum(event["kind"] == "user/message" for event in large_events) == 1
            finally:
                (output / "large-current.ansi").write_bytes(tui.transcript)
                tui.close()

            tui, screen, stream = launch(large_home, large_log)
            read, visible, wait, wait_new, wait_path, send, capture, resize = drive(
                tui, screen, stream
            )
            try:
                wait("LARGE-END", timeout=45)
                assert len(requests) == 2, "large replay sent a model request"
                send(b"/export large-replayed.txt\r")
                wait_path(workspace / "large-replayed.txt", timeout=45)
                large_replayed = (workspace / "large-replayed.txt").read_bytes()
                assert large_replayed == large_current
                (output / "large-replayed-export.txt").write_bytes(large_replayed)
                before = len(tui.transcript)
                send(b"/export\r")
                wait("Select export method")
                send(b"\r")
                wait("Selection is too large to copy")
                assert not OSC52.findall(tui.transcript[before:])
                capture("20-large-clipboard-refusal")
            finally:
                (output / "large-replayed.ansi").write_bytes(tui.transcript)
                tui.close()

            result = {
                "status": "passed",
                "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "provider_requests": len(requests),
                "durable_user_messages": sum(
                    event["kind"] == "user/message"
                    for event in [*seed_events, *large_events]
                ),
                "restart_replayed_without_request": True,
                "chooser_default_copy": True,
                "filename_escape_returns_to_chooser": True,
                "chooser_escape_cancels": True,
                "direct_file_bytes": len(exact_file),
                "clipboard_matches_file_exactly": True,
                "current_export_command_excluded": True,
                "extensionless_adds_txt": True,
                "missing_parents_created": True,
                "existing_target_not_overwritten": True,
                "directory_target_refused": True,
                "outside_root_refused": True,
                "host_clipboard_touched": False,
                "mouse_select_and_activate": True,
                "narrow_52x30": True,
                "light_theme": True,
                "no_color_sgr": True,
                "long_path_wrapped_and_committed": True,
                "large_file_bytes": len(large_current),
                "large_restart_bytes_exact": large_replayed == large_current,
                "large_clipboard_refused_without_osc52": True,
            }
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            (output / "result.json").write_text(json.dumps(result, indent=2))
    finally:
        if previous_ssh is None:
            os.environ.pop("SSH_CONNECTION", None)
        else:
            os.environ["SSH_CONNECTION"] = previous_ssh
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=2)
    print(f"PASS: {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("tmp/terminal-evidence/export-panel-pty-20260911T113419Z-e53ca2"),
    )
    arguments = parser.parse_args()
    run(arguments.binary.resolve(), arguments.output.resolve())
