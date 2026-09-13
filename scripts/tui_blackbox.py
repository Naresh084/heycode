#!/usr/bin/env python3
"""Drive the real heycode binary's FULL-SCREEN TUI through a PTY.

Every other automated check of the shell renders through ratatui's
`TestBackend` or the flat screen-reader projection. Both are the product's own
state machine talking to itself: neither proves that the binary, on a real
terminal, enters the alternate screen, negotiates bracketed paste, and paints
what the user is supposed to see. This does, by being a separate process that
knows nothing about heycode except its bytes.

Usage:
    python3 scripts/tui_blackbox.py [--binary target/debug/heycode]

Exits 0 when every scenario passed, 1 otherwise, 2 when the binary is missing.
Never touches the real `~/.heycode`: each scenario gets a fresh temporary home.
"""

from __future__ import annotations

import argparse
import fcntl
import http.server
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time
import threading

DEFAULT_BINARY = "target/debug/heycode"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\x1b[=>]|\x1b\][^\x07]*\x07")


class FullScreenTui:
    """One heycode process on a pseudo-terminal that claims to be xterm."""

    def __init__(self, home: str, cwd: str, binary: str, extra=(), rows=40, columns=110, fake=True, color=False, background=False, nonblocking_output=False):
        self.transcript = b""
        self.closed = False
        self.exit_status = None
        self.fake = fake
        self.pid, self.fd = pty.fork()
        if self.pid == 0:  # child
            os.environ["HEYCODE_HOME"] = home
            os.environ["TERM"] = "xterm-256color"
            if color:
                os.environ.pop("NO_COLOR", None)
                os.environ["COLORTERM"] = "truecolor"
            else:
                os.environ["NO_COLOR"] = "1"
            os.chdir(cwd)
            if nonblocking_output:
                os.set_blocking(1, False)
            os.execv(binary, [binary, *(["--fake"] if fake else []), * ([] if background else ["--no-background"]), "--trust-workspace", *extra])
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))

    def read(self, seconds: float) -> bytes:
        out = b""
        deadline = time.time() + seconds
        while time.time() < deadline:
            ready, _, _ = select.select([self.fd], [], [], 0.2)
            if not ready:
                continue
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk
        self.transcript += out
        return out

    def send(self, data: bytes, wait: float = 1.5) -> str:
        os.write(self.fd, data)
        return plain(self.read(wait))

    def resize(self, rows: int, columns: int) -> str:
        """Request a complete repaint when assertions need retained screen text."""
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        return plain(self.read(2))

    def alive(self) -> bool:
        try:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid == self.pid:
                self.exit_status = os.waitstatus_to_exitcode(status)
            return pid == 0
        except ChildProcessError:
            return False

    def close(self) -> None:
        if self.closed:
            return
        self.closed = True
        # A live delegated runtime needs the app's cancellation/shutdown path
        # before the terminal disappears. Never signal a reaped child's PID.
        if not self.fake:
            for _ in range(3):
                if not self.alive():
                    break
                try:
                    os.write(self.fd, b"\x03")
                    self.read(0.5)
                except OSError:
                    break
        try:
            if self.alive():
                os.kill(self.pid, signal.SIGKILL)
                # Release the PTY before reaping: macOS can keep a killed
                # terminal child in exit while its peer remains open.
                try:
                    os.close(self.fd)
                except OSError:
                    pass
                os.waitpid(self.pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass


def plain(raw: bytes) -> str:
    """Bytes as a reader sees them: escapes removed, text kept."""
    return ANSI.sub(b"", raw).decode("utf-8", errors="replace")


def shows(screen: str, expected: str) -> bool:
    """Whether `expected` appears on `screen`, ignoring spacing.

    Stripping escapes throws away the cursor moves that ratatui uses instead
    of spaces, so a painted "Ask anything" can arrive as "Askanything". This
    is not a terminal emulator and does not pretend to be one: it asks whether
    the characters appear in order, which is what these scenarios are for.
    """
    squash = lambda text: re.sub(r"\s+", "", text)  # noqa: E731
    return squash(expected) in squash(screen)


class Scenarios:
    """Each method is one scenario; `run` reports every failure it found."""

    def __init__(self, binary: str):
        self.binary = binary
        self.failures: list[str] = []

    def check(self, name: str, condition: bool, detail: str = "") -> None:
        if not condition:
            self.failures.append(f"{name}: {detail}" if detail else name)

    def start(self, stack, extra=()) -> tuple[FullScreenTui, str]:
        home = stack.enter(tempfile.mkdtemp(prefix="heycode-blackbox-home-"))
        workspace = stack.enter(tempfile.mkdtemp(prefix="heycode-blackbox-ws-"))
        tui = FullScreenTui(home, workspace, self.binary, extra=extra)
        stack.closers.append(tui.close)
        deadline = time.time() + 30
        while b"\x1b[?2004h" not in tui.transcript and time.time() < deadline:
            tui.read(0.2)
            if not tui.alive():
                break
        self.check("terminal ready", b"\x1b[?2004h" in tui.transcript,
                   "the binary did not enable bracketed paste before input")
        return tui, home

    def enters_the_alternate_screen_and_draws_a_composer(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(1)
        boot_raw = tui.transcript
        boot = plain(boot_raw)
        self.check(
            "alternate screen",
            b"\x1b[?1049h" in boot_raw,
            "the binary never asked for the alternate screen",
        )
        self.check(
            "bracketed paste",
            b"\x1b[?2004h" in boot_raw,
            "bracketed paste was not enabled",
        )
        self.check("composer prompt", "❯" in boot or "›" in boot, boot[-400:])
        self.check("status line", shows(boot, "Default"), boot[-400:])

    def a_paste_becomes_one_multiline_draft(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(6)
        screen = tui.send(b"\x1b[200~fix this:\nfn a() {}\n  b();\x1b[201~", wait=3)
        for line in ("fix this:", "fn a() {}", "b();"):
            self.check(
                "pasted line visible",
                shows(screen, line),
                f"{line!r} missing from {screen[-500:]!r}",
            )
        self.check(
            "paste sends nothing",
            not shows(screen, "FAKE-REPLY"),
            "the paste was submitted as a prompt",
        )

    def enter_sends_and_the_reply_is_painted(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(6)
        tui.send(b"hello there")
        screen = tui.send(b"\r", wait=6)
        self.check("reply painted", shows(screen, "FAKE-REPLY"), screen[-500:])

    def ctrl_c_warns_once_and_exits_on_the_second_press(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(6)
        screen = tui.send(b"\x03", wait=2)
        self.check("quit hint", shows(screen, "again to exit"), screen[-400:])
        self.check("still running", tui.alive(), "one Ctrl+C must not quit")
        tui.send(b"\x03", wait=2)
        deadline = time.time() + 5
        while tui.alive() and time.time() < deadline:
            time.sleep(0.2)
        self.check("second Ctrl+C exits", not tui.alive(), "the process was still running")

    def a_command_renders_its_card(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(6)
        screen = tui.send(b"/status\r", wait=4)
        for expected in ("runtime:", "workspace:"):
            self.check(
                "status card",
                shows(screen, expected),
                f"{expected!r} missing from {screen[-600:]!r}",
            )

    def typing_exit_like_commands_never_exits_or_panics(self, stack) -> None:
        for draft in (b"/exist", b"/exit"):
            tui, _ = self.start(stack)
            tui.read(4)
            for key in draft:
                tui.send(bytes([key]), wait=0.2)
                if not tui.alive():
                    self.check(
                        "typing command stays alive", False,
                        f"{draft!r} exited while typing {chr(key)!r}: "
                        f"{plain(tui.transcript)[-800:]!r}",
                    )
                    break
            else:
                self.check("typing command does not panic", "panicked at" not in plain(tui.transcript))
                if draft == b"/exist":
                    tui.send(b"\r", wait=2)
                    # Pet animation adds incremental cell updates; inspect a
                    # complete repaint instead of flattening those deltas.
                    screen = tui.resize(40, 111)
                    self.check("unknown command stays alive", tui.alive(), screen[-800:])
                    self.check("unknown command is reported", shows(screen, "unknown command"), screen[-800:])
                tui.close()

    def local_endpoint_input_stays_inside_connection_setup(self, stack) -> None:
        tui, _ = self.start(stack)
        tui.read(6)
        screen = tui.send(b"/connect\r", wait=3)
        self.check("connection choices", shows(screen, "Use a local model"), screen[-800:])
        tui.send(b"\x1b[B\r", wait=2)
        screen = tui.send(b"\r", wait=2)
        self.check("server URL field", shows(screen, "Server URL:"), screen[-800:])
        tui.send(b"\x15", wait=0.2)
        screen = tui.send(b"\x1b[200~http://localhost:2234\x1b[201~", wait=1)
        self.check("edited server URL", shows(screen, "http://localhost:2234"), screen[-800:])
        self.check("URL is not a prompt", not shows(plain(tui.transcript), "FAKE-REPLY"))
        tui.send(b"\x1b", wait=2)
        # Back uses incremental paint; unchanged title cells are absent from the delta.
        screen = tui.resize(40, 111)
        self.check("back to local connections", shows(screen, "Choose a local model server"), screen[-800:])

    def local_endpoint_key_is_masked_and_validated(self, stack) -> None:
        key = "blackbox-local-key-never-render"

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                authorized = self.headers.get("Authorization") == f"Bearer {key}"
                body = json.dumps({"models": [{
                    "type": "llm", "key": "test-model", "display_name": "Test local model",
                    "loaded_instances": [{"id": "test-loaded-model", "config": {"context_length": 4096}}],
                    "capabilities": {"trained_for_tool_use": True},
                }]}).encode()
                self.send_response(200 if authorized else 401)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        stack.closers.append(lambda: (server.shutdown(), server.server_close(), thread.join()))
        tui, home = self.start(stack)
        tui.read(6)
        tui.send(b"/connect\r", wait=2)
        tui.send(b"\x1b[B\r", wait=1)
        # The fixture serves LM Studio's catalog shape. Choose that provider
        # explicitly instead of depending on the order of connection rows.
        tui.send(b"LM Studio\r", wait=1)
        tui.send(b"\x15", wait=0.2)
        endpoint = f"http://127.0.0.1:{server.server_port}"
        tui.send(endpoint.encode(), wait=0.2)
        screen = tui.send(b"\x1b[B\r", wait=2)
        self.check("local masked prompt", shows(screen, "API key for"), screen[-800:])
        self.check("empty key has no placeholder bullets", "•" not in screen)
        invalid = "blackbox-invalid-key"
        tui.send(invalid.encode(), wait=0.3)
        screen = tui.send(b"\r", wait=2)
        self.check("invalid local key offers direct retry", shows(screen, "API key is invalid"), screen[-800:])
        credentials = os.path.join(home, "credentials.toml")
        self.check("invalid local key not saved", not os.path.exists(credentials) or invalid not in open(credentials).read())
        tui.send(key.encode(), wait=0.3)
        screen = tui.send(b"\r", wait=3)
        self.check("authenticated model choice", shows(screen, "test-loaded-model"), screen[-800:])
        self.check("local key never rendered", key not in plain(tui.transcript))
        credentials = os.path.join(home, "credentials.toml")
        self.check("validated local key saved", os.path.isfile(credentials) and key in open(credentials).read())

    def run(self) -> int:
        scenarios = [
            self.enters_the_alternate_screen_and_draws_a_composer,
            self.a_paste_becomes_one_multiline_draft,
            self.enter_sends_and_the_reply_is_painted,
            self.ctrl_c_warns_once_and_exits_on_the_second_press,
            self.a_command_renders_its_card,
            self.typing_exit_like_commands_never_exits_or_panics,
            self.local_endpoint_input_stays_inside_connection_setup,
            self.local_endpoint_key_is_masked_and_validated,
        ]
        failed_scenarios = 0
        for scenario in scenarios:
            before = len(self.failures)
            with Stack() as stack:
                try:
                    scenario(stack)
                except Exception as error:  # noqa: BLE001 - a crash is a failure
                    self.failures.append(f"{scenario.__name__}: raised {error!r}")
            status = "ok" if len(self.failures) == before else "FAILED"
            failed_scenarios += int(status == "FAILED")
            print(f"{scenario.__name__} ... {status}")
        for failure in self.failures:
            print(f"  - {failure}")
        print(f"\n{len(scenarios) - failed_scenarios} of "
              f"{len(scenarios)} scenarios reported no failure")
        return 1 if self.failures else 0


class Stack:
    """Temp directories and processes released even when a scenario throws."""

    def __init__(self):
        self.paths: list[str] = []
        self.closers: list = []

    def enter(self, path: str) -> str:
        self.paths.append(path)
        return path

    def __enter__(self) -> "Stack":
        return self

    def __exit__(self, *_exc) -> bool:
        for close in reversed(self.closers):
            close()
        for path in self.paths:
            shutil.rmtree(path, ignore_errors=True)
        return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=DEFAULT_BINARY)
    arguments = parser.parse_args()
    binary = os.path.abspath(arguments.binary)
    if not os.path.isfile(binary) or not os.access(binary, os.X_OK):
        print(f"no heycode binary at {binary}; build it with `cargo build` first", file=sys.stderr)
        return 2
    return Scenarios(binary).run()


if __name__ == "__main__":
    raise SystemExit(main())
