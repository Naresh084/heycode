#!/usr/bin/env python3
"""Actual /import terminal acceptance against an immutable, offline heycode build.

Every path and credential source belongs to a disposable child HOME. Screens
come from the emitted PTY cells; no test backend or synthetic UI is used.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

import pyte
from terminal_screenshot import TerminalByteStream, render_screen

PINNED_SHA = "449b25cf42720f30409b550b7847853f2db0543426c6c9fa33717df8559cb318"


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


class Terminal:
    def __init__(self, binary: Path, root: Path, output: Path, ordinal: int):
        self.output, self.ordinal = output, ordinal
        self.raw = bytearray()
        self.screen = Screen(138, 46)
        self.stream = TerminalByteStream(self.screen)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 46, 138, 0, 0))
        environment = {key: value for key, value in os.environ.items() if not (
            key.endswith(("_API_KEY", "_AUTH_TOKEN", "_ACCESS_TOKEN"))
            or key.startswith(("HEYCODE_", "ANTHROPIC_", "OPENAI_", "OPENROUTER_", "CLAUDE_CODE_OAUTH"))
        )}
        environment.update({"HOME": str(root / "os-home"), "HEYCODE_HOME": str(root / "heycode-home"),
                            "TERM": "xterm-256color", "COLORTERM": "truecolor", "TMPDIR": str(root / "temp")})
        environment.pop("NO_COLOR", None)
        self.process = subprocess.Popen([str(binary), "--fake", "--no-background", "--trust-workspace"],
            cwd=root / "workspace", env=environment, stdin=slave, stdout=slave, stderr=slave,
            start_new_session=True, close_fds=True)
        os.close(slave)
        self.wait("shift+tab to cycle")

    def read(self, seconds=0.15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], max(0, min(0.1, deadline-time.monotonic())))
            if ready:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                self.raw.extend(chunk)
                self.stream.feed(chunk)
        return "\n".join(self.screen.display)

    def wait(self, text: str, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            visible = self.read()
            if "".join(text.split()) in "".join(visible.split()):
                return visible
            if self.process.poll() is not None:
                raise AssertionError(f"CLI exited waiting for {text!r}:\n{visible}")
        raise AssertionError(f"Timed out waiting for {text!r}:\n{self.read()}")

    def command(self, text: str, expected: str):
        os.write(self.master, text.encode())
        self.read(0.2)
        os.write(self.master, b"\r")
        return self.wait(expected)

    def capture(self, name: str):
        visible = self.read(0.3)
        (self.output / f"{name}.txt").write_text(visible)
        (self.output / f"{name}.ansi").write_bytes(self.raw)
        render_screen(self.screen, self.output / f"{name}.png")
        print(f"captured {name}", flush=True)
        return visible

    def close(self):
        (self.output / f"terminal-{self.ordinal}.ansi").write_bytes(self.raw)
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)


def write(root: Path, path: str, text: str):
    destination = root / path
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(text)


def scan(terminal: Terminal, source: Path, product="codex"):
    visible = terminal.command(f"/import {product} {source}", "Unlisted rows are omitted.")
    return visible


def selection(terminal: Terminal, scan_text: str):
    # Names, row ids and statuses are observed from the actual terminal, not
    # guessed from the parser's internal enumeration order.
    rows = re.findall(r"(item-\d+)\s*\|\s*(?:Some\((\w+)\)|([\w ]+?))\s*\|\s*([^\n]+)", scan_text)
    chosen = []
    for item, legacy_kind, kind, row in rows:
        if "skip-agent" in row:
            chosen.append("keep:" + item)
        elif "collision" in row:
            chosen.append(item + "=renamed-agent")
        elif re.search(r"\|\s*Ready\s*\|", row):
            chosen.append(item)
    # Screen may retain older blocks; exact row ids must be selected once.
    chosen = list(dict.fromkeys(chosen))
    assert chosen, scan_text
    visible = terminal.command("/import select " + ",".join(chosen), "Confirm this exact selection")
    matches = re.findall(r"/import confirm\s+([a-f0-9]{64})", visible)
    if not matches:
        # A narrow terminal may wrap the digest; ANSI screen cells preserve
        # the final exact hexadecimal sequence on the following row.
        matches = re.findall(r"\b[a-f0-9]{64}\b", visible)
    assert matches, visible
    return matches[-1]


def generation(home: Path):
    pointer = home / "state/config-imports/current.json"
    if not pointer.exists():
        return None
    current = json.loads(pointer.read_text())
    objects = [p for p in pointer.parent.glob("*.json") if p.name != "current.json"]
    for path in objects:
        value = json.loads(path.read_text())
        if value.get("revision") == current.get("revision"):
            return value
    raise AssertionError(f"published generation not found: {current}")


def run(binary: Path, output: Path):
    binary = binary.resolve()
    actual_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert actual_sha == PINNED_SHA, actual_sha
    output.mkdir(parents=True, exist_ok=False)
    result = {"binary_sha256": actual_sha, "provider": "built-in offline fake", "commercial_requests": 0,
              "native_configuration_preserved": False, "checks": []}
    terminal = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-import-terminal-") as folder:
            root = Path(folder).resolve()
            for name in ["os-home", "heycode-home", "workspace", "source-home", "temp"]:
                (root / name).mkdir()
            home, source = root / "heycode-home", root / "source-home"
            write(home, "config.toml", 'schema_version = 31\n[ui]\naccent = "#224466"\n')
            write(home, "settings.toml", '# Native settings must stay unchanged\nschema_version = 1\n')
            write(home, "AGENTS.md", "NATIVE_USER_GUIDANCE")
            write(home, "agents/collision.json", '{"display":"Native collision","instructions":"NATIVE_COLLISION_GUIDANCE"}')
            write(home, "agents/skip-agent.json", '{"display":"Native keep","instructions":"NATIVE_KEEP_GUIDANCE"}')
            write(root, "workspace/AGENTS.md", "NATIVE_PROJECT_GUIDANCE")
            marker = root / "MCP_MUST_NOT_EXECUTE"
            write(source, ".codex/config.toml", f'''model = "foreign-model-needs-binding"
[mcp_servers.reader]
command = "/usr/bin/touch"
args = ["{marker}"]
[mcp_servers.secret]
command = "server"
env = {{ TOKEN = "IMPORT_SECRET_SENTINEL" }}
''')
            for name in ["collision", "skip-agent"]:
                write(source, f".codex/agents/{name}.toml", "name='Imported reader'\ndescription='Read source'\nsandbox_mode='read-only'\ndeveloper_instructions='FROZEN_AGENT_GUIDANCE'\n")
            write(source, ".codex/AGENTS.md", "IMPORTED_USER_GUIDANCE")
            write(source, ".agents/skills/import-review/SKILL.md", "---\nname: import-review\ndescription: Imported review\n---\nFROZEN_SKILL_GUIDANCE\n")
            write(source, ".gemini/commands/import-echo.toml", "description='Echo imported arguments'\nprompt='FROZEN_COMMAND {{args}}'\n")
            (source / ".codex/auth.json").symlink_to(root / "MUST_NOT_READ_CREDENTIALS")
            native_paths = [home / "config.toml", home / "settings.toml", home / "AGENTS.md", home / "agents/collision.json", home / "agents/skip-agent.json", root / "workspace/AGENTS.md"]
            native_bytes = {str(p): p.read_bytes() for p in native_paths}

            terminal = Terminal(binary, root, output, 1)
            terminal.capture("00-start")
            terminal.command("/import", "absolute source root")
            terminal.capture("01-help")
            inventory = scan(terminal, source)
            terminal.capture("02-read-only-inventory")
            assert "Conflict" in inventory and any(label in inventory for label in ["NeedsBinding", "Manual setup"])
            assert "IMPORT_SECRET_SENTINEL" not in inventory
            assert generation(home) is None
            digest = selection(terminal, inventory)
            terminal.capture("03-explicit-rename-keep-selection")
            terminal.command("/import cancel", "cancellation requested")
            terminal.command(f"/import confirm {digest}", "explicit confirmation")
            terminal.capture("04-cancelled-selection-refused")
            assert generation(home) is None
            result["checks"].append("scan and cancel created no import generation")

            inventory = scan(terminal, source)
            digest = selection(terminal, inventory)
            write(source, ".codex/AGENTS.md", "IMPORTED_USER_GUIDANCE_REVIEW_AGAIN")
            terminal.command(f"/import confirm {digest}", "preview is stale")
            terminal.capture("05-stale-source-confirmation-refused")
            assert generation(home) is None
            result["checks"].append("source edit invalidated exact confirmed digest without publication")
            inventory = scan(terminal, source)
            digest = selection(terminal, inventory)
            terminal.command(f"/import confirm {digest}", "Imported 4 resources in generation 1")
            terminal.capture("06-committed-recomposition-required")
            terminal.command("/import status", "Active import generation: 0")
            terminal.capture("07-old-world-remains-pinned")
            first = generation(home)
            assert first and first["revision"] == 1 and len(first["entries"]) == 4, first
            assert not marker.exists()
            (output / "generation-1.json").write_text(json.dumps(first, indent=2))
            terminal.close(); terminal = None
            write(source, ".codex/agents/collision.toml", "name='Changed source'\ndescription='Read source'\ndeveloper_instructions='UNREVIEWED_NEW_SOURCE'\n")

            terminal = Terminal(binary, root, output, 2)
            terminal.command("/import status", "Active import generation: 1")
            terminal.capture("08-restarted-generation-active")
            terminal.command("/agent-config show renamed-agent", "FROZEN_AGENT_GUIDANCE")
            shown = terminal.capture("09-frozen-imported-agent-active")
            assert "UNREVIEWED_NEW_SOURCE" not in shown
            terminal.command("/skills", "import-review")
            terminal.capture("10-imported-skill-native-picker")
            os.write(terminal.master, b"\x1b"); terminal.read(0.3)
            terminal.command("/mcp", "reader")
            terminal.capture("11-imported-mcp-disabled")
            assert not marker.exists()
            os.write(terminal.master, b"\x1b"); terminal.read(0.3)
            inventory = scan(terminal, source, "gemini")
            digest = selection(terminal, inventory)
            terminal.command(f"/import confirm {digest}", "Imported 1 resources in generation 2")
            terminal.capture("12-command-import-committed")
            terminal.close(); terminal = None
            write(source, ".gemini/commands/import-echo.toml", "prompt='UNREVIEWED_NEW_COMMAND'\n")

            terminal = Terminal(binary, root, output, 3)
            terminal.command("/import status", "Active import generation: 2")
            terminal.command('/import-echo raw "quoted argument"', "FAKE-REPLY: offline smoke response.")
            terminal.capture("13-frozen-command-native-dispatch")
            assert not marker.exists()
            for path, expected in native_bytes.items():
                assert Path(path).read_bytes() == expected, path
            result["native_configuration_preserved"] = True
            result["checks"].extend(["renamed imported agent activated after restart from frozen bytes", "native skill picker lists imported skill", "MCP remained disabled with no process marker", "imported Gemini command dispatched through actual offline native agent"])
            sessions = list(home.rglob("*.jsonl"))
            logged = "\n".join(path.read_text(errors="replace") for path in sessions)
            assert "FROZEN_COMMAND raw" in logged and "quoted argument" in logged
            assert "UNREVIEWED_NEW_COMMAND" not in logged
            result["checks"].append("durable native history contains reviewed command and exact user arguments")
            for index, path in enumerate(sessions):
                (output / f"session-{index}.jsonl").write_bytes(path.read_bytes())
            (output / "generation-2.json").write_text(json.dumps(generation(home), indent=2))
            result["passed"] = True
    except Exception as error:
        result["passed"] = False
        result["error"] = str(error)
        if terminal is not None:
            terminal.capture("failure")
        raise
    finally:
        if terminal is not None:
            terminal.close()
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    options = parser.parse_args()
    print(json.dumps(run(options.binary, options.output), indent=2))
