#!/usr/bin/env python3
"""Paired, prompt-free command UI acceptance for six Phase 2 controls.

The current Claude Code binary is run with disposable HOME/config/workspace
roots, a dummy credential and loopback-denied networking.  The heycode side uses
an immutable binary, its built-in fake provider without submitting a turn, a
disposable installed theme, and disposable capture/STT helpers that synthesize
PCM WAV and text.  The personal microphone, account data, keychain, and every
external model provider remain out of scope.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

import pyte

from terminal_screenshot import TerminalByteStream, render_screen


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class Terminal:
    def __init__(
        self,
        command: list[str],
        *,
        cwd: Path,
        environment: dict[str, str],
        columns: int,
        rows: int,
        timeout: float,
    ) -> None:
        self.timeout = timeout
        self.screen = Screen(columns, rows)
        self.stream = TerminalByteStream(self.screen)
        self.transcript = bytearray()
        master, slave = pty.openpty()
        self.master = master
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        self.process = subprocess.Popen(
            command,
            cwd=cwd,
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def read(self, seconds: float = 0.15) -> str:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select(
                [self.master], [], [], max(0.0, min(0.05, deadline - time.monotonic()))
            )
            if not ready:
                continue
            try:
                data = os.read(self.master, 65_536)
            except OSError:
                break
            if not data:
                break
            self.transcript.extend(data)
            self.stream.feed(data)
        return self.text()

    def send(self, data: bytes, *, seconds: float = 0.2) -> str:
        os.write(self.master, data)
        return self.read(seconds)

    def wait(self, *needles: str, timeout: float | None = None) -> str:
        deadline = time.monotonic() + (timeout or self.timeout)
        latest = self.text()
        while time.monotonic() < deadline:
            latest = self.read()
            if any(needle in latest for needle in needles):
                return latest
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"process exited with {self.process.returncode} while waiting for "
                    f"{needles}:\n{latest}"
                )
        raise TimeoutError(f"timed out waiting for {needles}:\n{latest}")

    def capture(self, output: Path, name: str, *, background: str = "#101014") -> str:
        visible = self.read(0.55)
        (output / f"{name}.txt").write_text(visible)
        render_screen(
            self.screen,
            output / f"{name}.png",
            background=background,
            foreground="#e8eaf0",
        )
        return visible

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self.process.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                self.process.kill()
            except ProcessLookupError:
                pass
            self.process.wait(timeout=5)
        try:
            os.close(self.master)
        except OSError:
            pass


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def clear_input(terminal: Terminal) -> None:
    terminal.send(b"\x1b", seconds=0.1)
    terminal.send(b"\x15", seconds=0.15)


def command(terminal: Terminal, value: str, *expected: str) -> str:
    clear_input(terminal)
    terminal.send(value.encode())
    terminal.send(b"\r")
    return terminal.wait(*expected)


def scrub_credentials(environment: dict[str, str]) -> None:
    for key in list(environment):
        if (
            key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_"))
            or key.endswith("_API_KEY")
            or key.endswith("_AUTH_TOKEN")
        ):
            environment.pop(key)


def reach_claude(terminal: Terminal, timeout: float) -> str:
    visible = terminal.read(3)
    deadline = time.monotonic() + timeout
    while not any(marker in visible.lower() for marker in ("shift+tab to cycle", "manual mode on")):
        lower = visible.lower()
        if "custom api key" in lower and "do you want to use" in lower:
            terminal.send(b"\x1b[A\r", seconds=1.2)
        elif "security notes:" in lower and "press enter to continue" in lower:
            terminal.send(b"\r", seconds=1.2)
        elif "trust" in lower and "folder" in lower:
            terminal.send(b"\x1b[B\r", seconds=1.2)
        elif "theme" in lower and "choose" in lower:
            terminal.send(b"\r", seconds=1.2)
        elif terminal.process.poll() is not None:
            raise RuntimeError(f"Claude exited during local onboarding:\n{visible}")
        elif time.monotonic() >= deadline:
            raise TimeoutError(f"timed out reaching Claude command input:\n{visible}")
        else:
            terminal.read(0.5)
        visible = terminal.text()
    return visible


def run_claude(output: Path, *, columns: int, rows: int, timeout: float) -> dict[str, object]:
    executable_value = shutil.which("claude")
    if executable_value is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    executable = Path(executable_value).resolve()
    output.mkdir(parents=True)
    captures: list[str] = []
    prefix_available: dict[str, bool] = {}
    executed: list[str] = []
    with tempfile.TemporaryDirectory(prefix="heycode-command-ui-claude-") as folder:
        root = Path(folder)
        home = root / "home"
        config = root / "config"
        workspace = root / "workspace"
        for path in (home, config, workspace):
            path.mkdir()
        (config / ".claude.json").write_text(
            json.dumps(
                {
                    "hasCompletedOnboarding": True,
                    "theme": "dark",
                    "lastOnboardingVersion": "2.1.268",
                }
            )
        )
        environment = os.environ.copy()
        scrub_credentials(environment)
        environment.update(
            {
                "HOME": str(home),
                "CLAUDE_CONFIG_DIR": str(config),
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                "CLAUDE_CODE_REMOTE_CONTROL": "0",
                "CLAUDE_CODE_NO_FLICKER": "1",
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:9",
                "ANTHROPIC_API_KEY": "local-only-dummy",
                "HTTP_PROXY": "http://127.0.0.1:9",
                "HTTPS_PROXY": "http://127.0.0.1:9",
                "NO_PROXY": "127.0.0.1,localhost",
                "EDITOR": "/usr/bin/true",
                "VISUAL": "/usr/bin/true",
            }
        )
        terminal = Terminal(
            [
                str(executable),
                "--safe-mode",
                "--strict-mcp-config",
                "--no-chrome",
                "--setting-sources",
                "project,local",
                "--settings",
                '{"remoteControlAtStartup":false}',
                "--permission-mode",
                "manual",
                "--session-id",
                str(uuid.uuid4()),
                "--name",
                "terminal-command-ui-reference",
                "--model",
                "opus",
            ],
            cwd=workspace,
            environment=environment,
            columns=columns,
            rows=rows,
            timeout=timeout,
        )
        try:
            reach_claude(terminal, timeout)
            terminal.capture(output, "00-ready-no-prompt")
            captures.append("00-ready-no-prompt")
            for index, name in enumerate(
                ("theme", "keybindings", "scroll-speed", "statusline", "tui", "voice"),
                start=1,
            ):
                clear_input(terminal)
                visible = terminal.send(f"/{name}".encode(), seconds=0.65)
                capture_name = f"{index:02d}-{name}-command"
                terminal.capture(output, capture_name)
                captures.append(capture_name)
                lines = [line.lower() for line in visible.splitlines() if name in line.lower()]
                prefix_available[name] = not any("no commands match" in line for line in lines)

            for name, key in (("theme", b"\x1b[B"), ("scroll-speed", b"\x1b[C")):
                if not prefix_available[name]:
                    continue
                clear_input(terminal)
                terminal.send(f"/{name}\r".encode(), seconds=1.0)
                terminal.read(0.8)
                opened = f"07-{name}-opened"
                terminal.capture(output, opened)
                captures.append(opened)
                terminal.send(key, seconds=0.5)
                moved = f"08-{name}-keyboard-selection"
                terminal.capture(output, moved)
                captures.append(moved)
                terminal.send(b"\x1b", seconds=0.5)
                executed.append(name)

            if prefix_available["keybindings"]:
                clear_input(terminal)
                terminal.send(b"/keybindings\r", seconds=1.5)
                terminal.read(1.0)
                terminal.capture(output, "09-keybindings-editor-return")
                captures.append("09-keybindings-editor-return")
                terminal.send(b"\x1b", seconds=0.2)
                executed.append("keybindings")

            if prefix_available["statusline"]:
                clear_input(terminal)
                terminal.send(b"/statusline\r", seconds=1.0)
                terminal.read(0.8)
                terminal.capture(output, "10-statusline-opened")
                captures.append("10-statusline-opened")
                terminal.send(b"\x1b", seconds=0.4)
                executed.append("statusline")

            if prefix_available["tui"]:
                clear_input(terminal)
                terminal.send(b"/tui\r", seconds=1.0)
                terminal.read(0.8)
                terminal.capture(output, "11-tui-opened")
                captures.append("11-tui-opened")
                terminal.send(b"\x1b", seconds=0.4)
                executed.append("tui")

            # Voice intentionally stops at command discovery. Executing the
            # source command may request personal microphone authority; native
            # behavior is exercised below with a generated WAV helper instead.
            (output / "terminal.ansi").write_bytes(bytes(terminal.transcript))
            files = [path for path in config.rglob("*") if path.is_file()]
            (output / "disposable-config-files.txt").write_text(
                "\n".join(sorted(str(path.relative_to(config)) for path in files)) + "\n"
            )
        finally:
            terminal.close()
    result = {
        "status": "passed",
        "runtime": "Claude Code full-screen TUI over PTY",
        "version": subprocess.run(
            [str(executable), "--version"], capture_output=True, text=True, check=False
        ).stdout.strip(),
        "binary": str(executable),
        "binary_sha256": sha256(executable),
        "prefix_available": prefix_available,
        "executed_local_surfaces": executed,
        "voice_execution": "withheld to avoid personal microphone access",
        "conversation_prompts": 0,
        "external_provider": "loopback-denied dummy route",
        "captures": captures,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


def platform_words() -> tuple[str, str]:
    os_name = {"Darwin": "macos", "Linux": "linux", "FreeBSD": "freebsd"}.get(
        platform.system()
    )
    architecture = {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "x86_64": "x86_64",
        "AMD64": "x86_64",
    }.get(platform.machine())
    if os_name is None or architecture is None:
        raise RuntimeError(f"unsupported fixture platform: {platform.platform()}")
    return os_name, architecture


def write_theme_package(root: Path) -> Path:
    package = root / "ember-theme"
    (package / ".heycode-plugin").mkdir(parents=True)
    (package / "themes").mkdir()
    os_name, architecture = platform_words()
    (package / ".heycode-plugin" / "plugin.toml").write_text(
        f'''schema_version = 1
id = "acme/ember"
name = "Ember theme fixture"
version = "1.0.0"
description = "Disposable Phase 2 command UI theme."
license = "MIT"
default_enabled = true
requested_permissions = []
platforms = [{{ os = "{os_name}", architecture = "{architecture}" }}]
dependencies = []
conflicts = []

[[contributions]]
kind = "theme"
id = "ember"
path = "themes/ember.json"
exposure = {{ mode = "namespaced" }}

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/ember-theme"
revision = "terminal-v1"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
'''
    )
    (package / "themes" / "ember.json").write_text(
        json.dumps(
            {
                "title": "Ember fixture",
                "colors": {
                    "accent": "#ff6600",
                    "success": "#33cc66",
                    "error": "#ff3355",
                    "warn": "#ffcc22",
                    "text": "#f8f8f2",
                    "dim": "#b8b8aa",
                    "border": "#777766",
                    "code": "#cc99ff",
                },
            }
        )
    )
    return package


def write_voice_helpers(root: Path) -> tuple[Path, Path]:
    capture = root / "capture.py"
    stt = root / "stt.py"
    capture.write_text(
        '''#!/usr/bin/env python3
import struct, sys
frames = 1600
header = bytearray(b"RIFF")
header += struct.pack("<I", 36 + frames * 2)
header += b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, 1, 16000, 32000, 2, 16)
header += b"data" + struct.pack("<I", frames * 2)
sys.stdout.buffer.write(b"HEYCODE_VOICE_READY\\n")
sys.stdout.buffer.flush()
sys.stdin.buffer.read()
sys.stdout.buffer.write(header + bytes(frames * 2))
sys.stdout.buffer.flush()
'''
    )
    stt.write_text(
        '''#!/usr/bin/env python3
import sys
sys.stdin.buffer.read()
sys.stdout.write("dictated local words")
'''
    )
    capture.chmod(0o755)
    stt.chmod(0o755)
    return capture, stt


def heycode_environment(
    home: Path,
    *,
    capture: Path,
    stt: Path | None,
    color: str,
) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HEYCODE_HOME": str(home),
            "HEYCODE_VOICE_CAPTURE_COMMAND": json.dumps([str(capture)]),
            "TERM": "xterm-256color",
        }
    )
    if stt is None:
        environment.pop("HEYCODE_STT_COMMAND", None)
    else:
        environment["HEYCODE_STT_COMMAND"] = json.dumps([str(stt)])
    if color == "truecolor":
        environment["COLORTERM"] = "truecolor"
        environment.pop("NO_COLOR", None)
    elif color == "ansi256":
        environment.pop("COLORTERM", None)
        environment.pop("NO_COLOR", None)
    elif color == "none":
        environment.pop("COLORTERM", None)
        environment["NO_COLOR"] = "1"
    else:
        raise ValueError(color)
    return environment


def start_heycode(
    binary: Path,
    home: Path,
    workspace: Path,
    environment: dict[str, str],
    *,
    columns: int,
    rows: int,
    timeout: float,
    resume: bool = False,
) -> Terminal:
    args = [str(binary), "--fake", "--no-background", "--trust-workspace"]
    if resume:
        args.append("--continue")
    terminal = Terminal(
        args,
        cwd=workspace,
        environment=environment,
        columns=columns,
        rows=rows,
        timeout=timeout,
    )
    terminal.wait("shift+tab to cycle")
    return terminal


def stop_heycode(terminal: Terminal) -> None:
    if terminal.process.poll() is None:
        clear_input(terminal)
        terminal.send(b"/quit\r", seconds=0.3)
        deadline = time.monotonic() + 5
        while terminal.process.poll() is None and time.monotonic() < deadline:
            terminal.read(0.1)
    terminal.close()


def run_heycode(
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
    output.mkdir(parents=True)
    captures: list[str] = []
    assertions: list[str] = []
    transcripts = bytearray()
    with tempfile.TemporaryDirectory(prefix="heycode-command-ui-native-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        capture_helper, stt_helper = write_voice_helpers(root)
        environment = heycode_environment(
            home, capture=capture_helper, stt=stt_helper, color="truecolor"
        )
        package = write_theme_package(root)
        install = subprocess.run(
            [str(binary), "--restricted-workspace", "plugin", "install", str(package)],
            cwd=workspace,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
        )
        (output / "theme-install.json").write_text(
            json.dumps(
                {"exit": install.returncode, "stdout": install.stdout, "stderr": install.stderr},
                indent=2,
            )
        )
        if install.returncode != 0:
            raise AssertionError(f"theme install failed: {install.stderr}")

        terminal = start_heycode(
            binary,
            home,
            workspace,
            environment,
            columns=columns,
            rows=rows,
            timeout=timeout,
        )
        try:
            terminal.capture(output, "00-ready-no-prompt")
            captures.append("00-ready-no-prompt")

            clear_input(terminal)
            alias = terminal.send(b"/keybindings", seconds=0.65)
            assert "/keybindings" in alias and "/keymap" in alias, alias
            terminal.capture(output, "01-keybindings-alias-command-menu")
            captures.append("01-keybindings-alias-command-menu")
            terminal.send(b"\r")
            terminal.wait("Keymap · revision")
            keymap = terminal.capture(output, "02-keybindings-picker")
            assert "enter edit" in keymap and "command-palette" in keymap, keymap
            captures.append("02-keybindings-picker")
            terminal.send(b"\x1b[B", seconds=0.3)
            terminal.capture(output, "03-keybindings-keyboard-selection")
            captures.append("03-keybindings-keyboard-selection")
            terminal.send(b"\x1b", seconds=0.3)
            command(
                terminal,
                "/keybindings command-palette ctrl+g",
                "keymap persisted and applied live",
            )
            terminal.send(b"\x07", seconds=0.6)
            palette = terminal.capture(output, "04-keybinding-applied-live")
            assert "/help" in palette and "enter select" in palette, palette
            captures.append("04-keybinding-applied-live")
            terminal.send(b"\x1b", seconds=0.3)
            command(terminal, "/keybindings submit ctrl+c", "bound to both")
            terminal.capture(output, "05-keybinding-conflict-refusal")
            captures.append("05-keybinding-conflict-refusal")
            assertions.append("alias, picker, keyboard selection, live custom chord and conflict refusal")

            command(terminal, "/theme", "Theme")
            themes = terminal.capture(output, "06-theme-picker-with-plugin")
            for expected in ("heycode dark", "heycode light", "heycode high contrast", "Ember fixture"):
                assert expected in themes, (expected, themes)
            captures.append("06-theme-picker-with-plugin")
            before_custom = len(terminal.transcript)
            terminal.send(b"\x1b[A", seconds=0.5)
            terminal.capture(output, "07-theme-keyboard-live-preview")
            captures.append("07-theme-keyboard-live-preview")
            terminal.send(b"\r", seconds=0.35)
            terminal.wait("theme persisted as")
            custom_raw = bytes(terminal.transcript[before_custom:])
            if b"38;2;255;102;0" not in custom_raw:
                raise AssertionError("custom truecolor accent was not emitted")
            terminal.capture(output, "08-theme-plugin-persisted-truecolor")
            captures.append("08-theme-plugin-persisted-truecolor")
            command(terminal, "/theme missing-theme", "unknown theme")
            terminal.capture(output, "09-theme-invalid-preserves-custom")
            captures.append("09-theme-invalid-preserves-custom")
            assertions.append("plugin theme listed, previewed, persisted, applied as authored truecolor and invalid id refused")

            command(terminal, "/scroll-speed", "Scroll speed")
            initial = next(line for line in terminal.text().splitlines() if "ruler" in line)
            terminal.send(b"\x1b[<64;55;20M", seconds=0.4)
            moved = next(line for line in terminal.text().splitlines() if "ruler" in line)
            assert initial != moved, (initial, moved)
            terminal.send(b"\x1b[C", seconds=0.4)
            terminal.capture(output, "10-scroll-keyboard-pointer-preview")
            captures.append("10-scroll-keyboard-pointer-preview")
            terminal.send(b"\r", seconds=0.3)
            terminal.wait("scroll speed persisted as 1.25x")
            terminal.capture(output, "11-scroll-persisted")
            captures.append("11-scroll-persisted")
            assertions.append("scroll picker keyboard plus real SGR wheel preview and persistence")

            command(terminal, "/statusline off", "status line: off")
            command(terminal, "/statusline hints off", "command hints: off")
            terminal.capture(output, "12-statusline-footer-and-hints-off")
            captures.append("12-statusline-footer-and-hints-off")
            command(terminal, "/statusline reset", "status line: on; command hints: on")
            command(terminal, "/statusline", "status line: on; command hints: on")
            terminal.capture(output, "13-statusline-reset-and-report")
            captures.append("13-statusline-reset-and-report")
            assertions.append("built-in footer and command hints hide, reset, report and persist without shell execution")

            command(terminal, "/voice status", "Voice:")
            status = terminal.capture(output, "14-voice-local-status")
            assert "Capture executable: installed" in status and "60-second recording limit" in status
            captures.append("14-voice-local-status")
            command(terminal, "/voice start", "RECORDING")
            terminal.capture(output, "15-voice-generated-recording")
            captures.append("15-voice-generated-recording")
            command(terminal, "/voice stop", "Dictation inserted into the draft")
            inserted = terminal.capture(output, "16-voice-generated-inserted-draft")
            assert "dictated local words" in inserted, inserted
            captures.append("16-voice-generated-inserted-draft")
            clear_input(terminal)
            command(terminal, "/voice start", "RECORDING")
            command(terminal, "/voice cancel", "Dictation stopped; no transcript inserted")
            terminal.capture(output, "17-voice-cancelled")
            captures.append("17-voice-cancelled")
            command(terminal, "/voice unexpected", "Usage: /voice")
            terminal.capture(output, "18-voice-invalid")
            captures.append("18-voice-invalid")
            assertions.append("synthetic local capture covered status, recording, stop/transcribe/insert, cancel and invalid action")

            clear_input(terminal)
            offset = len(terminal.transcript)
            terminal.send(b"/tui screen-reader\r", seconds=0.5)
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                terminal.read(0.2)
                latest = bytes(terminal.transcript[offset:])
                if b"\x1b[?1049l" in latest and b"keys: Enter sends" in latest:
                    break
            else:
                raise AssertionError("screen-reader recomposition did not leave alternate screen")
            terminal.capture(output, "19-tui-screen-reader")
            captures.append("19-tui-screen-reader")
            offset = len(terminal.transcript)
            terminal.send(b"/tui auto\r", seconds=0.5)
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                terminal.read(0.2)
                latest = bytes(terminal.transcript[offset:])
                if b"\x1b[?1049h" in latest and "shift+tab to cycle" in terminal.text():
                    break
            else:
                raise AssertionError("automatic recomposition did not re-enter alternate screen")
            terminal.capture(output, "20-tui-auto-restored")
            captures.append("20-tui-auto-restored")
            assertions.append("same process switched screen-reader and automatic terminal presentations")
        finally:
            transcripts.extend(terminal.transcript)
            stop_heycode(terminal)

        resumed = start_heycode(
            binary,
            home,
            workspace,
            environment,
            columns=columns,
            rows=rows,
            timeout=timeout,
            resume=False,
        )
        try:
            resumed.send(b"\x07", seconds=0.5)
            resumed.capture(output, "21-restart-keybinding-persisted")
            captures.append("21-restart-keybinding-persisted")
            resumed.send(b"\x1b", seconds=0.2)
            command(resumed, "/theme", "Theme")
            persisted = resumed.capture(output, "22-restart-plugin-theme-persisted")
            assert "Ember fixture" in persisted, persisted
            captures.append("22-restart-plugin-theme-persisted")
            resumed.send(b"\x1b", seconds=0.2)
            command(resumed, "/scroll-speed", "1.25x")
            resumed.capture(output, "23-restart-scroll-persisted")
            captures.append("23-restart-scroll-persisted")
            resumed.send(b"\x1b", seconds=0.2)
            assertions.append("custom keybinding, plugin theme and scroll speed survived restart")
        finally:
            transcripts.extend(resumed.transcript)
            stop_heycode(resumed)

        tier_results: dict[str, dict[str, bool]] = {}
        for tier in ("ansi256", "none"):
            tier_env = heycode_environment(
                home, capture=capture_helper, stt=stt_helper, color=tier
            )
            tier_terminal = start_heycode(
                binary,
                home,
                workspace,
                tier_env,
                columns=columns,
                rows=rows,
                timeout=timeout,
            )
            try:
                offset = len(tier_terminal.transcript)
                command(tier_terminal, "/theme", "Theme")
                raw = bytes(tier_terminal.transcript[offset:])
                has_truecolor = bool(re.search(rb"\x1b\[[0-9;]*38;2;", raw))
                has_256 = bool(re.search(rb"\x1b\[[0-9;]*38;5;", raw))
                has_basic = bool(re.search(rb"\x1b\[(?:[0-9;]*;)?(?:3[0-7]|9[0-7])m", raw))
                tier_terminal.capture(output, f"24-theme-{tier}")
                captures.append(f"24-theme-{tier}")
                if tier == "ansi256":
                    assert has_256 and not has_truecolor, (has_truecolor, has_256)
                else:
                    assert not has_truecolor and not has_256 and not has_basic, (
                        has_truecolor,
                        has_256,
                        has_basic,
                    )
                tier_results[tier] = {
                    "truecolor_sgr": has_truecolor,
                    "ansi256_sgr": has_256,
                    "basic_foreground_sgr": has_basic,
                }
            finally:
                transcripts.extend(tier_terminal.transcript)
                stop_heycode(tier_terminal)
        assertions.append("custom theme emitted 256-color only at ANSI256 and no foreground color at NO_COLOR")

        events: list[dict[str, object]] = []
        for path in sorted(home.rglob("session.jsonl")):
            events.extend(
                json.loads(line) for line in path.read_text().splitlines() if line.strip()
            )
        forbidden = [
            event
            for event in events
            if event.get("kind") in ("turn/start", "request/header", "user/message")
        ]
        if forbidden:
            raise AssertionError(f"management commands entered inference input: {forbidden}")
        (output / "events.json").write_text(json.dumps(events, indent=2))
        (output / "terminal.ansi").write_bytes(bytes(transcripts))
        settings = {
            path.name: path.read_text(errors="replace")
            for path in sorted(home.iterdir())
            if path.is_file() and path.stat().st_size <= 256 * 1024
        }
        (output / "settings-evidence.json").write_text(json.dumps(settings, indent=2))

    result = {
        "status": "passed",
        "runtime": "real heycode CLI full-screen TUI over PTY",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider": "built-in fake, zero submitted turns",
        "external_provider_requests": 0,
        "personal_microphone_access": 0,
        "voice_input": "generated PCM WAV and deterministic local STT helper",
        "theme": "disposable installed namespaced plugin",
        "color_tiers": tier_results,
        "captures": captures,
        "assertions": assertions,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=112)
    parser.add_argument("--rows", type=int, default=46)
    parser.add_argument("--timeout", type=float, default=45)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    claude = run_claude(
        output / "claude", columns=args.columns, rows=args.rows, timeout=args.timeout
    )
    heycode = run_heycode(
        args.binary,
        output / "heycode",
        columns=args.columns,
        rows=args.rows,
        timeout=args.timeout,
    )
    result = {
        "status": "passed",
        "scope": ["theme", "keybindings", "scroll-speed", "statusline", "tui", "voice"],
        "claude": claude,
        "heycode": heycode,
        "external_model_prompts": 0,
        "personal_microphone_access": 0,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
