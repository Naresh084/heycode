#!/usr/bin/env python3
"""Scripted Claude session-management state reference over seeded disposable history.

Captures the reachable `/resume`, `/branch`, `/rewind` and `/clear` presentation
states without submitting a conversation prompt. Inherited credentials are
removed, a dummy key is supplied and every provider/proxy target is a refusing
loopback address, so a state that would need a model round-trip is recorded as
"reference unavailable" instead of being forced.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
import time
import uuid

from claude_compaction_terminal_reference import ClaudePty
from terminal_screenshot import render_screen

SEEDED_PROMPTS = ("Lifecycle first checkpoint", "Lifecycle second checkpoint")
ORIGINAL_TITLE = "Lifecycle source original"
COMPANION_TITLE = "Lifecycle source companion"
UNKNOWN_TITLE = "Lifecycle absent conversation"

# Only these command lines may ever reach the source binary.
ALLOWED_COMMANDS = {
    "/rewind",
    "/resume",
    "/branch",
    "/clear",
    f"/resume {ORIGINAL_TITLE}",
    f"/resume {UNKNOWN_TITLE}",
}

KEYS = {
    "enter": b"\r",
    "up": b"\x1b[A",
    "down": b"\x1b[B",
    "escape": b"\x1b",
    "tab": b"\t",
}


def _seed_history(log: Path, session_id: str, workspace: Path, version: str) -> None:
    """Append explicit local user/assistant pairs; no model produced them."""
    existing = [json.loads(line) for line in log.read_text().splitlines() if line]
    parent = next((row["uuid"] for row in reversed(existing) if "uuid" in row), None)
    with log.open("a") as stream:
        for index, prompt in enumerate(SEEDED_PROMPTS):
            user_id, assistant_id = str(uuid.uuid4()), str(uuid.uuid4())
            shared = {
                "isSidechain": False,
                "userType": "external",
                "cwd": str(workspace.resolve()),
                "sessionId": session_id,
                "version": version,
                "gitBranch": "",
                "timestamp": f"2026-09-12T00:00:0{index}.000Z",
                "fixtureSeeded": True,
            }
            user = {
                **shared,
                "parentUuid": parent,
                "type": "user",
                "message": {"role": "user", "content": prompt},
                "uuid": user_id,
            }
            assistant = {
                **shared,
                "parentUuid": user_id,
                "type": "assistant",
                "uuid": assistant_id,
                "requestId": f"local_fixture_{index}",
                "message": {
                    "model": "local-fixture-no-inference",
                    "id": f"msg_local_fixture_{index}",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "text",
                            "text": "Synthetic local response; no model was called.",
                        }
                    ],
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                    },
                },
            }
            for row in (user, assistant):
                stream.write(json.dumps(row) + "\n")
            parent = assistant_id


def run(output: Path, *, theme: str, color: bool, columns: int, rows: int) -> dict[str, object]:
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which("claude")).resolve()
    requests: list[dict[str, object]] = []
    captures: list[dict[str, object]] = []
    unavailable: list[dict[str, str]] = []
    result: dict[str, object] = {
        "engine": "claude-code",
        "source_binary": str(executable),
        "sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
        "version": "2.1.269",
        "prompt_sent": False,
        "history": "two explicitly seeded local user/assistant pairs",
        "theme": theme,
        "color": color,
        "viewport": {"columns": columns, "rows": rows},
        "commercial_calls": 0,
    }
    terminal = None

    class RefuseRequests(http.server.BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802 - http.server contract
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            requests.append({"method": "POST", "path": self.path})
            self.send_error(403, "Inference is forbidden in this state reference")

        def do_GET(self):  # noqa: N802 - http.server contract
            requests.append({"method": "GET", "path": self.path})
            self.send_error(403, "External services are outside this state reference")

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), RefuseRequests)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    background = "#f8f9fb" if theme == "light" else "#101014"
    foreground = "#20242c" if theme == "light" else "#dddddd"
    with tempfile.TemporaryDirectory(prefix="claude-lifecycle-states-") as folder:
        root = Path(folder)
        config, workspace = root / "config", root / "workspace"
        config.mkdir()
        workspace.mkdir()
        (config / ".claude.json").write_text(
            json.dumps(
                {
                    "hasCompletedOnboarding": True,
                    "theme": theme,
                    "lastOnboardingVersion": executable.name,
                }
            )
        )
        env = os.environ.copy()
        # Drop every inherited Anthropic/Claude marker: a leaked child-session or
        # credential variable would silently disable transcripts or reuse an account.
        for key in list(env):
            if key.startswith(("ANTHROPIC_", "CLAUDE")) or key.endswith(("_API_KEY", "_AUTH_TOKEN")):
                env.pop(key)
        env.update(
            CLAUDE_CONFIG_DIR=str(config),
            TERM="xterm-256color",
            CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
            CLAUDE_CODE_REMOTE_CONTROL="0",
            CLAUDE_CODE_NO_FLICKER="1",
            ANTHROPIC_BASE_URL=f"http://127.0.0.1:{server.server_port}",
            ANTHROPIC_API_KEY="local-only-dummy",
            HTTP_PROXY="http://127.0.0.1:9",
            HTTPS_PROXY="http://127.0.0.1:9",
            NO_PROXY="127.0.0.1,localhost",
        )
        env.pop("NO_COLOR", None)
        if color:
            env["COLORTERM"] = "truecolor"
        else:
            env["NO_COLOR"] = "1"
            env.pop("COLORTERM", None)
        common = [
            "--safe-mode",
            "--strict-mcp-config",
            "--no-chrome",
            "--setting-sources",
            "project,local",
            "--settings",
            '{"remoteControlAtStartup":false}',
            "--permission-mode",
            "manual",
            "--model",
            "opus",
        ]
        session_id = str(uuid.uuid4())
        companion_id = str(uuid.uuid4())
        marks = {"offset": 0}

        def capture(name: str, note: str | None = None) -> str:
            visible = terminal.read(0.9)
            (output / f"{name}.txt").write_text(visible)
            (output / f"{name}.ansi").write_bytes(bytes(terminal.transcript[marks["offset"] :]))
            marks["offset"] = len(terminal.transcript)
            render_screen(
                terminal.screen,
                output / f"{name}.png",
                background=background,
                foreground=foreground,
            )
            row = {"capture": name}
            if note:
                row["note"] = note
            captures.append(row)
            return visible

        def command(text: str) -> None:
            assert text in ALLOWED_COMMANDS, f"Unreviewed command refused: {text}"
            terminal.send(b"\x15")  # Discard any unsent fixture draft, never history.
            terminal.send(text.encode() + b"\r")
            terminal.read(1.2)

        def press(key: str, repeat: int = 1) -> None:
            for _ in range(repeat):
                terminal.send(KEYS[key])
                terminal.read(0.4)
            terminal.read(0.6)

        def type_text(text: str) -> None:
            for character in text:
                terminal.send(character.encode())
                terminal.read(0.12)
            terminal.read(0.8)

        def settle_to_composer() -> None:
            """Leave any modal before the next command is typed into the composer.

            A stray key would otherwise land in a still-open picker's search box
            instead of the composer and silently invalidate the next capture.
            """
            for _ in range(6):
                visible = terminal.read(0.6)
                if not any(marker in visible for marker in ("Resume session", "Rewind", "⌕ ")):
                    return
                terminal.send(KEYS["escape"])
            raise AssertionError(f"A modal stayed open:\n{terminal.visible()}")

        def record_unavailable(state: str, reason: str) -> None:
            unavailable.append({"state": state, "reason": reason})

        try:
            # Fresh, unseeded session: the only place an empty rewind list exists.
            terminal = ClaudePty(
                str(executable),
                env,
                workspace,
                [*common, "--session-id", session_id, "--name", ORIGINAL_TITLE],
                columns=columns,
                rows=rows,
            )
            terminal.ready()
            command("/rewind")
            visible = capture("00-rewind-empty-picker")
            if "rewind" not in visible.lower():
                record_unavailable(
                    "rewind empty picker",
                    "the source did not present a rewind surface on an empty conversation",
                )
            press("escape")
            capture("01-rewind-empty-cancelled")
            terminal.send(b"\x15")
            terminal.send(b"/copy\r")
            terminal.wait_for("No assistant message to copy")
            terminal.send(b"\x15")
            terminal.send(b"/exit\r")
            deadline = time.monotonic() + 5
            while terminal.process.poll() is None and time.monotonic() < deadline:
                terminal.read()
            terminal.close()
            (output / "bootstrap.ansi").write_bytes(bytes(terminal.transcript))
            terminal = None

            logs = list(config.glob(f"projects/**/{session_id}.jsonl"))
            assert len(logs) == 1, logs
            _seed_history(logs[0], session_id, workspace, executable.name)
            (output / "seeded-session.jsonl").write_bytes(logs[0].read_bytes())

            # A second saved conversation in the same project is what makes the
            # resume list show rows instead of an empty-project notice.
            terminal = ClaudePty(
                str(executable),
                env,
                workspace,
                [*common, "--session-id", companion_id, "--name", COMPANION_TITLE],
                columns=columns,
                rows=rows,
            )
            terminal.ready()
            terminal.send(b"/copy\r")
            terminal.wait_for("No assistant message to copy")
            terminal.send(b"\x15")
            terminal.send(b"/exit\r")
            deadline = time.monotonic() + 5
            while terminal.process.poll() is None and time.monotonic() < deadline:
                terminal.read()
            terminal.close()
            (output / "companion-bootstrap.ansi").write_bytes(bytes(terminal.transcript))
            terminal = None
            companion_logs = list(config.glob(f"projects/**/{companion_id}.jsonl"))
            assert len(companion_logs) == 1, companion_logs
            _seed_history(companion_logs[0], companion_id, workspace, executable.name)
            (output / "seeded-companion-session.jsonl").write_bytes(companion_logs[0].read_bytes())

            terminal = ClaudePty(
                str(executable),
                env,
                workspace,
                [*common, "--resume", session_id],
                columns=columns,
                rows=rows,
            )
            terminal.ready()
            marks["offset"] = len(terminal.transcript)
            capture("02-resumed-seeded-history")

            command("/resume")
            visible = capture("03-resume-bare-picker")
            picker_open = COMPANION_TITLE.lower() in visible.lower()
            if not picker_open:
                record_unavailable(
                    "bare resume picker",
                    "the source did not list saved conversations in this isolated configuration",
                )
            type_text("Lifecycle")
            capture("04-resume-picker-search")
            press("down")
            capture("05-resume-picker-selection-down")
            press("up")
            capture("06-resume-picker-selection-up")
            press("escape")
            capture("07-resume-picker-cancelled")
            settle_to_composer()

            command(f"/resume {ORIGINAL_TITLE}")
            capture("08-resume-exact-title")
            settle_to_composer()
            command(f"/resume {UNKNOWN_TITLE}")
            capture("09-resume-unknown-title")
            settle_to_composer()

            command("/branch")
            visible = capture("10-branch")
            if "unknown" in visible.lower() or "/branch" not in visible:
                record_unavailable(
                    "branch command",
                    "the source registry does not advertise /branch at this version",
                )
            settle_to_composer()

            command("/rewind")
            visible = capture("11-rewind-populated-picker")
            if "rewind" not in visible.lower():
                record_unavailable(
                    "populated rewind picker",
                    "the source did not present a rewind surface over seeded history",
                )
            press("up")
            capture("12-rewind-row-selected")
            press("enter")
            visible = capture("13-rewind-confirmation-choices")
            press("down")
            capture("14-rewind-confirmation-second-choice")
            press("escape")
            capture("15-rewind-confirmation-cancelled")
            press("escape")
            capture("16-rewind-cancelled")

            command("/clear")
            capture("17-clear")
            result["status"] = "captured"
        except Exception as error:  # noqa: BLE001 - evidence capture must record the failure
            result.update(status="failed", failure=f"{type(error).__name__}: {error}")
            if terminal is not None:
                try:
                    capture("failed")
                except Exception:  # noqa: BLE001
                    pass
        finally:
            if terminal is not None:
                terminal.close()
                (output / "terminal.ansi").write_bytes(bytes(terminal.transcript))
                result["owned_terminal_alive"] = terminal.process.poll() is None
            server.shutdown()
            server.server_close()
            server_thread.join(timeout=3)
            journals = {
                str(path.relative_to(config)): path.read_text()
                for path in config.glob("projects/**/*.jsonl")
            }
            (output / "journals.json").write_text(json.dumps(journals, indent=2))
            result.update(
                companion_session_id=companion_id,
                captures=captures,
                reference_unavailable=unavailable,
                loopback_requests=requests,
                source_session_id=session_id,
            )
            result.setdefault("status", "stopped")
            (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("dark", "light"), default="dark")
    parser.add_argument("--no-color", action="store_true")
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=42)
    arguments = parser.parse_args()
    outcome = run(
        arguments.output,
        theme=arguments.theme,
        color=not arguments.no_color,
        columns=arguments.columns,
        rows=arguments.rows,
    )
    if outcome["status"] == "failed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
