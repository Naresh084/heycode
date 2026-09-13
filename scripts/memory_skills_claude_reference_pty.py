#!/usr/bin/env python3
"""Capture Claude Code memory, skill, and skill-doctor reference surfaces.

The child always gets a disposable workspace, memory files, and skill roots.
It normally also gets disposable HOME and CLAUDE_CONFIG_DIR roots; the explicit
account-context mode inherits only the existing login so account-gated local
surfaces can be compared. The journey never submits a model prompt or opens a
memory source for editing.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
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


ROWS = 46
COLUMNS = 126


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def write_skill(root: Path, name: str, description: str) -> None:
    directory = root / name
    directory.mkdir(parents=True)
    (directory / "SKILL.md").write_text(
        "---\n"
        f"name: {name}\n"
        f"description: {description}\n"
        "---\n"
        f"DISPOSABLE_{name.upper()}_BODY\n"
    )


def run(output: Path, *, inherit_account: bool = False, theme: str = "dark", color: bool = True, token_order_probe: bool = False, input_probe: bool = False) -> dict[str, object]:
    if inherit_account and theme != "dark":
        raise ValueError("Theme references require a disposable configuration")
    output.mkdir(parents=True, exist_ok=True)
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude Code is unavailable")

    transcript = bytearray()
    captures: list[str] = []
    blocker: str | None = None
    input_observations = {}
    process: subprocess.Popen[bytes] | None = None
    master: int | None = None

    with tempfile.TemporaryDirectory(prefix="claude-memory-skills-reference-") as folder:
        root = Path(folder)
        disposable_home = root / "home"
        config = root / "claude-config"
        workspace = root / "workspace"
        disposable_home.mkdir()
        config.mkdir()
        workspace.mkdir()
        (config / ".claude.json").write_text(
            json.dumps(
                {
                    "hasCompletedOnboarding": True,
                    "theme": theme,
                    "lastOnboardingVersion": "2.1.268",
                }
            )
        )
        (workspace / "CLAUDE.md").write_text("DISPOSABLE_PROJECT_MEMORY\n")
        skills = workspace / ".claude" / "skills"
        write_skill(skills, "reference-alpha", "Disposable alpha reference")

        environment = os.environ.copy()
        environment.update({"TERM": "xterm-256color", "COLORTERM": "truecolor"})
        if not inherit_account:
            for key in list(environment):
                if key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")) or key.endswith(("_API_KEY", "_AUTH_TOKEN")):
                    environment.pop(key)
            environment.update(
                {
                    "HOME": str(disposable_home),
                    "CLAUDE_CONFIG_DIR": str(config),
                    "ANTHROPIC_API_KEY": "local-only-dummy",
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9",
                    "HTTP_PROXY": "http://127.0.0.1:9",
                    "HTTPS_PROXY": "http://127.0.0.1:9",
                    "NO_PROXY": "127.0.0.1,localhost",
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                    "CLAUDE_CODE_REMOTE_CONTROL": "0",
                }
            )
        environment.pop("NO_COLOR", None)
        environment.pop("CLAUDE_CODE_NO_FLICKER", None)
        if not color:
            environment["NO_COLOR"] = "1"
        environment.pop("CLAUDE_CODE_DISABLE_AUTO_MEMORY", None)
        session_id = str(uuid.uuid4())
        arguments = [
            executable,
            "--permission-mode",
            "manual",
            "--strict-mcp-config",
            "--no-chrome",
            "--setting-sources",
            "project,local",
            "--session-id",
            session_id,
            "--name",
            "isolated-memory-skills-reference",
            "--model",
            "opus",
        ]

        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", ROWS, COLUMNS, 0, 0),
        )
        process = subprocess.Popen(
            arguments,
            cwd=workspace,
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
        )
        os.close(slave)
        screen = Screen(COLUMNS, ROWS)
        stream = TerminalByteStream(screen)

        def read(seconds: float) -> str:
            assert master is not None
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                ready, _, _ = select.select(
                    [master], [], [], max(0.0, min(0.1, deadline - time.monotonic()))
                )
                if not ready:
                    continue
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                transcript.extend(data)
                stream.feed(data)
            return "\n".join(screen.display)

        def visible() -> str:
            return "\n".join(screen.display)

        def send(data: bytes) -> None:
            assert master is not None
            os.write(master, data)

        def capture(name: str) -> str:
            text = read(0.5)
            (output / f"{name}.txt").write_text(text)
            # Keep the exact ANSI prefix and cell-decoration runs used for the
            # PNG. This makes visual-reference regressions distinguishable
            # from emulator or rasterizer artifacts without replaying a live
            # session or altering the captured UI.
            (output / f"{name}.ansi").write_bytes(transcript)
            decorations: dict[str, object] = {}
            for attribute in ("underscore", "strikethrough"):
                runs: list[dict[str, int]] = []
                for row in range(screen.lines):
                    start: int | None = None
                    for column in range(screen.columns + 1):
                        active = column < screen.columns and bool(
                            getattr(screen.buffer[row][column], attribute)
                        )
                        if active and start is None:
                            start = column
                        elif not active and start is not None:
                            runs.append(
                                {"row": row, "start": start, "end": column - 1}
                            )
                            start = None
                decorations[attribute] = {
                    "cells": sum(run["end"] - run["start"] + 1 for run in runs),
                    "runs": runs,
                }
            (output / f"{name}.cells.json").write_text(
                json.dumps(decorations, indent=2)
            )
            render_screen(screen, output / f"{name}.png", background="#f8f9fb" if theme == "light" else "#101014", foreground="#20242c" if theme == "light" else "#dddddd")
            captures.append(name)
            return text

        def wait_for_any(needles: tuple[str, ...], timeout: float = 20.0) -> str:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                text = read(0.2)
                normalized = "".join(text.split()).lower()
                if any("".join(needle.split()).lower() in normalized for needle in needles):
                    return text
                if process is not None and process.poll() is not None:
                    break
            raise AssertionError(
                f"timed out waiting for one of {needles!r}: {visible()[-1800:]}"
            )

        def type_command(command: str) -> None:
            send(command.encode())
            read(0.7)

        def resize(columns: int, rows: int) -> None:
            screen.resize(lines=rows, columns=columns)
            assert master is not None
            fcntl.ioctl(
                master,
                termios.TIOCSWINSZ,
                struct.pack("HHHH", rows, columns, 0, 0),
            )
            if process is not None and process.poll() is None:
                os.killpg(process.pid, signal.SIGWINCH)
            read(1.2)

        try:
            initial = ""
            for _ in range(4):
                initial = wait_for_any(("custom API key", "trust this folder", "Try", "shift+tab", "? for shortcuts"), timeout=20.0)
                if "custom api key" in initial.lower():
                    send(b"\x1b[A\r")
                    read(1.0)
                elif "trust this folder" in initial.lower():
                    send(b"\x1b[B\r")
                    read(1.0)
                else:
                    break
            capture("00-start")
            if any(
                marker in initial.lower()
                for marker in ("welcome to claude code", "select a theme")
            ):
                raise RuntimeError(
                    "isolated Claude configuration reached an onboarding wall"
                )
            wait_for_any(("Try", "shift+tab", "? for shortcuts", "opus"), timeout=15.0)

            type_command("/memory")
            capture("01-memory-command-menu")
            send(b"\r")
            read(1.5)
            memory = capture("02-memory-open")
            if "memory" not in memory.lower():
                raise AssertionError("/memory did not expose a recognizable local surface")
            send(b"\x1b")
            read(0.6)

            type_command("/reload-skills")
            capture("03-reload-skills-command-menu")
            write_skill(skills, "reference-beta", "Disposable beta reference" + (" with a deliberately longer catalog description" * 20 if token_order_probe else ""))
            send(b"\r")
            read(1.5)
            reloaded = capture("04-reload-skills-result")
            if "reload" not in reloaded.lower() and "skill" not in reloaded.lower():
                raise AssertionError("/reload-skills did not expose a recognizable result")

            type_command("/skills")
            capture("05-skills-command-menu")
            send(b"\r")
            read(1.5)
            skill_list = capture("06-skills-after-reload")
            if "reference-alpha" not in skill_list or "reference-beta" not in skill_list:
                raise AssertionError(
                    "the isolated project skill fixtures were not visible after reload"
                )
            resize(60, 24)
            narrow_skills = capture("06a-skills-after-reload-narrow")
            if "skills" not in narrow_skills.lower() or "reference-alpha" not in narrow_skills:
                raise AssertionError("the Claude skills panel did not survive 60x24")
            resize(COLUMNS, ROWS)

            send(b"/")
            read(0.3)
            send(b"\x1b[200~beta\x1b[201~")
            read(0.7)
            searched_skills = capture("06b-skills-search")
            if "reference-beta" not in searched_skills:
                raise AssertionError("Claude skills search did not retain the matching fixture")
            send(b"\x7f\x7f\x7f\x7f")
            read(0.5)
            send(b"\x1b")
            read(0.5)
            send(b"\r")
            read(0.8)
            toggled_skills = capture("06c-skills-cycled-name-only")
            if "reference-alpha" not in toggled_skills or "name-only" not in toggled_skills:
                raise AssertionError("Claude skills Enter did not show its name-only state")
            send(b"\r")
            read(0.8)
            toggled_skills = capture("06d-skills-cycled-user-only")
            if "reference-alpha" not in toggled_skills or "user-only" not in toggled_skills:
                raise AssertionError("Claude skills Enter did not show its user-only state")
            send(b"\r")
            read(0.8)
            toggled_skills = capture("06e-skills-cycled-off")
            if "reference-alpha" not in toggled_skills or "off" not in toggled_skills.lower():
                raise AssertionError("Claude skills Enter did not show its off state")
            send(b"\r")
            read(0.8)
            capture("06f-skills-cycled-on")
            send(b"t")
            read(0.8)
            sorted_skills = capture("06g-skills-sort-key")
            if token_order_probe:
                rows = sorted_skills.split("Search skills", 1)[-1]
                if rows.find("reference-beta") > rows.find("reference-alpha"):
                    raise AssertionError("Token sort does not place the larger beta catalog entry first")
            if input_probe:
                target_row = next(i for i,line in enumerate(sorted_skills.splitlines()) if "reference-alpha" in line and "tok" in line)
                send(f"\x1b[<0;28;{target_row+1}M\x1b[<0;28;{target_row+1}m".encode())
                read(.5)
                clicked = capture("06h-skills-pointer")
                send(b"\x1b[200~UNFOCUSED_SKILL_PASTE\x1b[201~")
                read(.5)
                pasted = capture("06i-skills-unfocused-paste")
                input_observations = {
                    "selected_before": [line.strip() for line in sorted_skills.splitlines() if "❯" in line],
                    "selected_after_click": [line.strip() for line in clicked.splitlines() if "❯" in line],
                    "paste_visible": "UNFOCUSED_SKILL_PASTE" in pasted,
                    "panel_retained": "Skills" in pasted and "type to filter" in pasted,
                }
                send(b"\x1b")
                read(.3)
                cleared = capture("06j-skills-search-cleared")
                send(b"\r")
                read(.3)
                selected = capture("06k-skills-search-selection")
                send(b"\x1b")
                read(.3)
                dismissed = capture("06l-skills-dismissed")
                input_observations.update({
                    "esc_clears_query_keeps_search": "UNFOCUSED_SKILL_PASTE" not in cleared and "type to filter" in cleared,
                    "enter_leaves_search_without_cycling": "enter/space to cycle" in selected and "name-only" not in selected,
                    "esc_from_selection_closes": "Search skills" not in dismissed,
                })
            else:
                send(b"\x1b")
                read(0.3)
    
                type_command("/skill-doctor")
                capture("07-skill-doctor-command-menu")
                send(b"\r")
                read(3.0)
                doctor = capture("08-skill-doctor-result")
                if "skill" not in doctor.lower() or not any(
                    marker in doctor.lower()
                    for marker in ("token", "usage", "report", "couldn't compute")
                ):
                    raise AssertionError(
                        "/skill-doctor did not expose a recognizable local report"
                    )

            if process.poll() is not None:
                raise RuntimeError(
                    f"Claude Code exited during the reference journey: {process.returncode}"
                )
        except Exception as error:  # preserve a screenshotable, explicit boundary
            blocker = f"{type(error).__name__}: {error}"
            capture("blocked")
        finally:
            (output / "terminal.ansi").write_bytes(transcript)
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            os.close(master)

        result = {
            "engine": "claude",
            "theme": theme,
            "color": color,
            "terminal_renderer": "installed default; CLAUDE_CODE_NO_FLICKER unset",
            "version": subprocess.run(
                [executable, "--version"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip(),
            "status": "blocked" if blocker else "captured",
            "blocker": blocker,
            "viewport": {"columns": COLUMNS, "rows": ROWS},
            "captures": captures,
            "input_observations": input_observations,
            "model_prompt_sent": False,
            "isolated_configuration_roots": not inherit_account,
            "account_context_inherited": inherit_account,
            "user_setting_sources_enabled": False,
            "fixture": {
                "project_memory": "CLAUDE.md",
                "initial_skill": "reference-alpha",
                "added_before_reload": "reference-beta",
                "token_order_probe": token_order_probe,
            },
        }
        (output / "result.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
        return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("dark", "light"), default="dark")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--token-order-probe", action="store_true")
    parser.add_argument("--input-probe", action="store_true")
    parser.add_argument(
        "--inherit-account",
        action="store_true",
        help="inherit only the existing Claude account context; the workspace and skill fixtures remain disposable",
    )
    arguments = parser.parse_args()
    outcome = run(arguments.output, inherit_account=arguments.inherit_account, theme=arguments.theme, color=not arguments.no_color, token_order_probe=arguments.token_order_probe, input_probe=arguments.input_probe)
    if outcome["status"] != "captured":
        raise SystemExit(1)
