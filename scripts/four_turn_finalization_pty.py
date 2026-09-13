#!/usr/bin/env python3
"""Isolate whether a fourth ordinary loopback turn finishes in the real TUI."""

from __future__ import annotations

import argparse
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time

from memory_skills_pty import MODEL, Screen, write_skill
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


def run(
    binary: Path,
    output: Path,
    skills_sequence: bool,
    skill_interleave: bool,
) -> dict[str, object]:
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []
    response_events: list[dict[str, object]] = []
    response_lock = threading.Lock()

    def write_json(name: str, value: object) -> None:
        (output / name).write_text(json.dumps(value, indent=2))

    class Handler(http.server.BaseHTTPRequestHandler):
        post_number: int | None = None

        def respond(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path.endswith("/key"):
                self.respond({"data": {"label": "four-turn-loopback"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            length = int(self.headers["Content-Length"])
            request = json.loads(self.rfile.read(length))
            requests.append(request)
            number = len(requests)
            self.post_number = number
            write_json("requests.json", requests)
            with response_lock:
                response_events.append(
                    {
                        "request": number,
                        "event": "headers",
                        "close_connection": self.close_connection,
                    }
                )
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            chunks = [
                {
                    "id": f"four-turn-{number}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"content": f"FOUR_TURN_REPLY_{number}"},
                            "finish_reason": None,
                        }
                    ],
                },
                {
                    "id": f"four-turn-{number}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {"index": 0, "delta": {}, "finish_reason": "stop"}
                    ],
                },
            ]
            try:
                for chunk in chunks:
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
                with response_lock:
                    response_events.append(
                        {
                            "request": number,
                            "event": "flushed_done",
                            "close_connection": self.close_connection,
                        }
                    )
            except (BrokenPipeError, ConnectionResetError) as error:
                with response_lock:
                    response_events.append(
                        {
                            "request": number,
                            "event": "client_closed",
                            "error": type(error).__name__,
                        }
                    )
            finally:
                write_json("response-events.json", response_events)

        def finish(self):
            try:
                super().finish()
            finally:
                if self.post_number is not None:
                    with response_lock:
                        response_events.append(
                            {
                                "request": self.post_number,
                                "event": "socket_finished",
                                "close_connection": self.close_connection,
                            }
                        )
                    write_json("response-events.json", response_events)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    tui: FullScreenTui | None = None
    try:
        root = Path(tempfile.mkdtemp(prefix="fixture-root-", dir=output))
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        if skills_sequence or skill_interleave:
            write_skill(
                workspace / ".heycode" / "skills",
                "alpha",
                "Alpha catalog description",
                "ALPHA_BODY",
            )
        base = f"http://127.0.0.1:{server.server_port}/api/v1"
        (home / "config.toml").write_text(
            "schema_version = 31\n"
            "[llm]\n"
            'provider = "openrouter"\n'
            f'model = "{MODEL["id"]}"\n'
            'api_key_env = "HEYCODE_FOUR_TURN_FIXTURE"\n'
            f'base_url = "{base}"\n'
        )
        os.environ["HEYCODE_FOUR_TURN_FIXTURE"] = "local-only-not-a-real-key"
        tui = FullScreenTui(
            str(home),
            str(workspace),
            str(binary.resolve()),
            fake=False,
            color=True,
            rows=46,
            columns=126,
            extra=[
                "--provider",
                "openrouter",
                "--model",
                MODEL["id"],
                "--set",
                f"llm.base_url={base}",
                "--set",
                "llm.api_key_env=HEYCODE_FOUR_TURN_FIXTURE",
            ],
        )
        screen = Screen(126, 46)
        stream = TerminalByteStream(screen)

        def read(seconds: float = 0.2) -> None:
            stream.feed(tui.read(seconds))

        def visible() -> str:
            return "\n".join(screen.display)

        def wait_for(needle: str, timeout: float) -> bool:
            compact = "".join(needle.split())
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                read()
                if compact in "".join(visible().split()):
                    return True
                if not tui.alive():
                    return False
            return False

        def send_turn(command: str, number: int) -> bool:
            os.write(tui.fd, command.encode() + b"\r")
            if not wait_for(f"FOUR_TURN_REPLY_{number}", 45):
                return False
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                read()
                if "Responding…" not in visible():
                    return True
            return False

        if not wait_for("shift+tab to cycle", 45):
            raise AssertionError("CLI did not reach the initial prompt")

        stalled_turn: int | None = None
        if skill_interleave:
            if not send_turn("INTERLEAVE_ORDINARY_ONE", 1):
                stalled_turn = 1
            if stalled_turn is None and not send_turn(
                "/skill alpha INTERLEAVE_EXPLICIT", 2
            ):
                stalled_turn = 2
            if stalled_turn is None and not send_turn("INTERLEAVE_ORDINARY_TWO", 3):
                stalled_turn = 3
        elif skills_sequence:
            if not send_turn("ON_CATALOG_REQUEST", 1):
                stalled_turn = 1
            if stalled_turn is None:
                os.write(tui.fd, b"/skills\r")
                if not wait_for("1 skill", 15):
                    raise AssertionError("skills panel did not open")
                os.write(tui.fd, b"\r")
                if not wait_for("alpha is now name-only", 15):
                    raise AssertionError("name-only state did not apply")
                os.write(tui.fd, b"\x1b")
                read(0.5)
            if stalled_turn is None and not send_turn("NAME_ONLY_CATALOG_REQUEST", 2):
                stalled_turn = 2
            if stalled_turn is None and not send_turn(
                "/skill alpha NAME_ONLY_SKILL_REQUEST", 3
            ):
                stalled_turn = 3
            if stalled_turn is None:
                os.write(tui.fd, b"/skills\r")
                if not wait_for("1 skill", 15):
                    raise AssertionError("skills panel did not reopen")
                os.write(tui.fd, b"\r")
                if not wait_for("alpha is now user-only", 15):
                    raise AssertionError("user-only state did not apply")
                os.write(tui.fd, b"\x1b")
                read(0.5)
            if stalled_turn is None and not send_turn("USER_ONLY_CATALOG_REQUEST", 4):
                stalled_turn = 4
        else:
            for number in range(1, 5):
                if not send_turn(f"ORDINARY_TURN_{number}", number):
                    stalled_turn = number
                    break

        read(0.5)
        (output / "final-screen.txt").write_text(visible())
        render_screen(screen, output / "final-screen.png")
        durable_events: list[dict[str, object]] = []
        for path in sorted((home / "sessions").rglob("session.jsonl")):
            durable_events.extend(
                json.loads(line) for line in path.read_text().splitlines() if line.strip()
            )
        write_json("session-events.json", durable_events)
        turn_end_events = sum(event.get("kind") == "turn/end" for event in durable_events)
        result = {
            "status": "passed" if stalled_turn is None else "stalled",
            "stalled_turn": stalled_turn,
            "provider_requests": len(requests),
            "flushed_done": sum(
                event["event"] == "flushed_done" for event in response_events
            ),
            "socket_finished": sum(
                event["event"] == "socket_finished" for event in response_events
            ),
            "durable_turn_end_events": turn_end_events,
            "provider": "localhost deterministic SSE fixture",
            "scenario": (
                "on, name-only, explicit skill, user-only"
                if skills_sequence
                else (
                    "ordinary, explicit skill, ordinary"
                    if skill_interleave
                    else "four ordinary turns without skill-state mutations"
                )
            ),
            "fixture_root": str(root),
        }
        write_json("result.json", result)
        return result
    finally:
        if tui is not None:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--skills-sequence", action="store_true")
    parser.add_argument("--skill-interleave", action="store_true")
    arguments = parser.parse_args()
    if arguments.skills_sequence and arguments.skill_interleave:
        parser.error("choose at most one sequence")
    print(
        json.dumps(
            run(
                arguments.binary,
                arguments.output,
                arguments.skills_sequence,
                arguments.skill_interleave,
            ),
            indent=2,
        )
    )
