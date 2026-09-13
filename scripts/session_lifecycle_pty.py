#!/usr/bin/env python3
"""Bounded native lifecycle journey on an immutable CLI with the offline provider.

The two prompt turns are explicit local fake responses. Commands must add no
model requests. All sessions, workspaces and process ownership are disposable.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
import time

from command_reference_pty import Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


def run(binary: Path, sha256: str, output: Path, *, theme: str = "heycode-dark", color: bool = True,
        columns: int = 110, rows: int = 42):
    binary = binary.resolve()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == sha256, "CLI pin changed"
    output.mkdir(parents=True, exist_ok=False)
    captures, actions = [], []
    result = {"binary": str(binary), "sha256": sha256, "provider": "offline fake", "commercial_calls": 0,
              "engine": "heycode", "prompt_sent": False, "theme": theme, "color": color,
              "viewport": {"columns": columns, "rows": rows}}
    background = "#f8f9fb" if theme == "heycode-light" else "#101014"
    foreground = "#20242c" if theme == "heycode-light" else "#dddddd"
    with tempfile.TemporaryDirectory(prefix="session-lifecycle-") as folder:
        root = Path(folder)
        home, work = root / "home", root / "work"
        home.mkdir(); work.mkdir()
        (home / "settings.toml").write_text(f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n')
        tui = FullScreenTui(str(home), str(work), str(binary), fake=True, color=color, rows=rows, columns=columns)
        screen = Screen(columns, rows)
        stream = TerminalByteStream(screen)

        def read():
            stream.feed(tui.read(.1))
            return "\n".join(screen.display)

        def records():
            return {path.parent.name: [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                    for path in home.rglob("session.jsonl")}

        def count(kind):
            return sum(event.get("kind") == kind for events in records().values() for event in events)

        def wait(predicate, description, timeout=30):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                visible = read()
                if predicate(visible):
                    return visible
                if not tui.alive():
                    raise AssertionError(f"CLI exited: {description}")
            raise AssertionError(f"Timeout: {description}\n{read()}")

        def send(data, label):
            actions.append(label)
            os.write(tui.fd, data)
            read()

        def command(text):
            send(text.encode() + b"\r", text)

        def capture(name):
            visible = read()
            (output / f"{name}.txt").write_text(visible)
            render_screen(screen, output / f"{name}.png", background=background, foreground=foreground)
            captures.append(name)
            return visible

        def click(text, exclude=None, modal=False):
            visible = read()
            start = next((i for i, line in enumerate(screen.display) if line.strip() == "Rewind"), -1) if modal else -1
            matches = [(i, line) for i, line in enumerate(screen.display) if i > start and text in line and (exclude is None or exclude not in line)]
            assert len(matches) == 1, (text, matches, visible)
            row, line = matches[0]
            column = line.index(text)
            send(f"\x1b[<0;{column+1};{row+1}M\x1b[<0;{column+1};{row+1}m".encode(), f"mouse select {text}")

        try:
            idle = lambda text: "for shortcuts" in text or "shift+tab to cycle" in text
            wait(idle, "startup")
            command("/rewind")
            wait(lambda text: "Nothing to rewind to yet." in text, "empty rewind picker")
            capture("00-rewind-empty")
            send(b"\x1b", "cancel empty rewind")
            command("/name Lifecycle original")
            wait(lambda text: "Renamed to Lifecycle original" in text, "rename alias receipt")
            capture("01-name-alias")
            original_id = next(iter(records()))
            for index, prompt in enumerate(("Lifecycle first checkpoint", "Lifecycle second checkpoint"), 1):
                send(prompt.encode() + b"\r", f"offline prompt {index}")
                wait(lambda text: count("turn/end") == index and "FAKE-REPLY" in text, f"offline turn {index} settled")
            before_branch = records()[original_id]
            turn_ids = [event["data"]["turn"] for event in before_branch if event.get("kind") == "turn/start"]
            assert len(turn_ids) == 2, turn_ids
            older_turn, newer_turn = turn_ids
            baseline_requests = count("request/header")
            assert count("user/message") == 2
            command("/branch Lifecycle branch")
            wait(lambda text: len(records()) == 2 and 'Branched conversation "Lifecycle branch"' in text and original_id in text, "named branch restart and full receipt")
            branch_id = next(identifier for identifier in records() if identifier != original_id)
            assert any(event.get("kind") == "session/title" and event.get("data", {}).get("title") == "Lifecycle branch" for event in records()[branch_id])
            assert records()[original_id][:len(before_branch)] == before_branch
            receipt = " ".join(line.strip() for line in read().splitlines())
            assert branch_id in receipt and f'/resume {original_id} ("Lifecycle original")' in receipt
            assert not any("✗" in line and "Branched conversation" in line for line in read().splitlines())
            assert any("❯ /branch Lifecycle branch" in line for line in read().splitlines()), "branch command did not survive UI recomposition"
            assert any("⎿" in line and "Branched conversation" in line for line in read().splitlines()), "branch result lacks command receipt styling"
            assert count("user/message") == 2, "branch command leaked into durable conversation"
            capture("02-branch-named")

            restart_offset = len(tui.transcript)
            command("/resume Lifecycle original")
            wait(lambda text: b"\x1b[?1049h" in tui.transcript[restart_offset:]
                 and "Lifecycle original" in text and "Enter resume" not in text, "exact title automatically resumes original")
            capture("03-resume-exact-title")
            command("/resume Lifecycle")
            wait(lambda text: "Session Lifecycle was not found." in text, "partial title refusal")
            capture("04-resume-partial-not-found")
            command("/resume")
            wait(lambda text: "Enter resume" in text, "bare resume picker")
            capture("05-resume-bare-picker")
            picker_rows = [line for line in screen.display if line.lstrip().startswith("❯ ")]
            assert any("Lifecycle branch" in line for line in picker_rows), picker_rows
            assert not any("Lifecycle original" in line for line in picker_rows), picker_rows
            assert "Lifecycle original" not in read().split("Resume session", 1)[1], "current session leaked into picker"
            send(b"\x01", "show all project groups")
            wait(lambda text: "Ctrl+A to show this project" in text, "all project scope hint")
            assert str(work) in read(), "all-project group must name its exact workspace"
            capture("05a-resume-all-projects")
            send(b"\x01", "return to this project")
            wait(lambda text: "Ctrl+A to show all projects" in text, "current project scope hint")
            send(b"\x12", "open selected saved session rename")
            wait(lambda text: "rename  Lifecycle branch" in text, "rename shortcut from focused search")
            capture("05b-resume-rename-shortcut")
            send(b"\x1b", "cancel rename without changing saved title")
            send(b"Lifecycle branch", "type saved branch into focused resume picker")
            wait(lambda text: "Lifecycle branch" in text and "search" in text.lower(), "picker substring search")
            capture("06-resume-picker-search")
            for _ in range(10):
                if any(line.lstrip().startswith("❯ Lifecycle branch") for line in screen.display):
                    break
                send(b"\x1b[B", "select displayed saved branch row")
            else:
                raise AssertionError("Saved branch row was not selectable")
            restart_offset = len(tui.transcript)
            send(b"\r", "select picker search match")
            wait(lambda text: b"\x1b[?1049h" in tui.transcript[restart_offset:]
                 and "Lifecycle branch" in text and "Enter resume" not in text, "picker selection resumed saved branch")
            capture("07-resume-picker-selected")
            command(f"/resume {original_id}")
            wait(lambda text: "Lifecycle original" in text and "Lifecycle first checkpoint" in text, "return before explicit branch resume")
            restart_offset = len(tui.transcript)
            command(f"/resume {branch_id}")
            wait(lambda text: b"\x1b[?1049h" in tui.transcript[restart_offset:]
                 and "Lifecycle branch" in text, "explicit saved branch resume")
            capture("08-resume-explicit-id")

            command("/rewind")
            wait(lambda text: "(current)" in text and "⚠ No code restore" in text, "populated rewind picker with unavailable forked file checkpoint")
            capture("09-rewind-populated-current")
            assert any("❯" in line and "(current)" in line for line in screen.display)
            click("Lifecycle first checkpoint", modal=True)
            capture("10-rewind-row-mouse-current-unchanged")
            assert any("❯" in line and "(current)" in line for line in screen.display)
            assert len(records()) == 2, "Mouse selection mutated a session"
            send(b"\x1b[A\r", "select newest checkpoint and choose restoration")
            wait(lambda text: "1. Restore conversation" in text and "Never mind" in text, "restoration choices")
            capture("11-rewind-confirm-conversation-default")
            click("Never mind")
            wait(lambda text: "1. Restore conversation" not in text and idle(text), "explicit cancellation")
            assert len(records()) == 2
            capture("12-rewind-cancelled")
            command("/rewind")
            wait(lambda text: "(current)" in text, "reopen rewind")
            send(b"\x1b[A\r", "choose newest checkpoint")
            wait(lambda text: "1. Restore conversation" in text, "reopened restoration choices")
            send(b"\x1b[200~/quit\n\x1b[201~", "paste consumed by rewind modal")
            capture("13-rewind-paste-consumed")
            assert len(records()) == 2 and tui.alive()
            restart_offset = len(tui.transcript)
            click("1. Restore conversation")
            wait(lambda text: len(records()) == 3 and b"\x1b[?1049h" in tui.transcript[restart_offset:]
                 and any("Lifecycle second checkpoint" in line for line in screen.display[-8:]), "rewind child and restored composer draft")
            capture("14-rewind-completed-draft")
            assert count("user/message") == 2, "Rewind submitted its restored draft"
            assert count("request/header") == baseline_requests, "Lifecycle command started inference"
            send(b"\x15", "clear fixture restored draft")
            command("/rewind 999")
            wait(lambda text: "unknown rewind turn" in text, "invalid rewind refusal")
            capture("15-rewind-unknown-turn")
            assert len(records()) == 3
            restart_offset = len(tui.transcript)
            command("/clear")
            wait(lambda text: len(records()) == 4 and b"\x1b[?1049h" in tui.transcript[restart_offset:]
                 and "Lifecycle first checkpoint" not in text, "fresh clear session")
            capture("16-clear-fresh-session")
            command(f"/resume {original_id}")
            wait(lambda text: "Lifecycle original" in text and "Lifecycle first checkpoint" in text, "recover original after clear")
            capture("17-clear-resume-original")
            assert count("user/message") == 2
            assert count("request/header") == baseline_requests
            result.update(status="passed", fake_prompt_turns=2, request_headers=baseline_requests,
                          original_id=original_id, branch_id=branch_id, durable_sessions=len(records()))
        except Exception as error:
            result.update(status="failed", failure=f"{type(error).__name__}: {error}")
            capture("failed")
        finally:
            (output / "journals.json").write_text(json.dumps(records(), indent=2))
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
            result.update(captures=captures, actions=actions, owned_process_alive=tui.alive())
            (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
    if result["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    args = parser.parse_args()
    run(args.binary, args.sha256, args.output, theme=args.theme, color=not args.no_color,
        columns=args.columns, rows=args.rows)
