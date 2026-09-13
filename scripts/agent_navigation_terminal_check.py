#!/usr/bin/env python3
"""Exercise native agent navigation through the real heycode CLI and a PTY.

The provider is a deterministic localhost SSE fixture, but the CLI, native
agent runtime, task source, terminal renderer, keyboard handling and mouse hit
regions are production code. Every run uses disposable heycode/work directories.
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import http.server
import json
import os
import re
import struct
import tempfile
import termios
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import pyte

from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Local agent navigation fixture",
    "created": 1787752741,
    "description": "Local deterministic terminal fixture",
    "context_length": 131072,
    "architecture": {
        "input_modalities": ["text"],
        "output_modalities": ["text"],
        "tokenizer": "Other",
        "instruct_type": None,
    },
    "pricing": {"prompt": "0", "completion": "0"},
    "top_provider": {
        "context_length": 131072,
        "max_completion_tokens": 32768,
        "is_moderated": False,
    },
    "supported_parameters": [
        "max_tokens",
        "temperature",
        "tool_choice",
        "tools",
        "reasoning",
    ],
    "reasoning": {
        "mandatory": True,
        "default_enabled": True,
        "supported_efforts": ["max", "high", "low"],
        "default_effort": "max",
    },
}
AGENTS = ("Atlas", "Boreal", "Cygnus", "Draco", "Equinox")


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(
    binary: Path,
    output: Path,
    *,
    theme: str,
    color: bool,
    columns: int,
    rows: int,
    cards_only: bool = False,
) -> dict[str, object]:
    capture_started_utc = datetime.now(timezone.utc).isoformat()
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []
    agents = AGENTS[:1] if cards_only else AGENTS
    release = {name: threading.Event() for name in agents}
    live = {name: threading.Event() for name in agents}

    class Handler(http.server.BaseHTTPRequestHandler):
        def respond(self, value: object) -> None:
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path.endswith("/key"):
                self.respond({"data": {"label": "terminal-local-fixture"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            messages = request.get("messages", [])
            user_text = [
                str(message.get("content", ""))
                for message in messages
                if message.get("role") == "user"
                and not str(message.get("content", "")).startswith("[job ")
            ]
            joined = "\n".join(user_text)
            tooltail = any(message.get("role") == "tool" for message in messages)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()

            def chunk(delta: dict[str, object], finish: str | None = None) -> None:
                if "tool_calls" in delta:
                    delta["reasoning"] = "Dispatching the requested native agent or read tool."
                payload = {
                    "id": f"terminal-navigation-{len(requests)}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                }
                self.wfile.write(("data: " + json.dumps(payload) + "\n\n").encode())
                self.wfile.flush()

            def tool(
                name: str,
                arguments: dict[str, object],
                index: int = 0,
                identity: str | None = None,
            ) -> dict[str, object]:
                return {
                    "index": index,
                    "id": identity or f"terminal-{name}-{len(requests)}-{index}",
                    "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments)},
                }

            try:
                child = next(
                    (name for name in agents if f"PHASE2_CHILD_{name.upper()}" in joined),
                    None,
                )
                if child and not tooltail:
                    chunk(
                        {
                            "tool_calls": [
                                tool(
                                    "read",
                                    {"path": "activity-fixture.txt"},
                                    identity=f"read-{child.lower()}",
                                )
                            ]
                        }
                    )
                    chunk({}, "tool_calls")
                elif child:
                    chunk(
                        {
                            "content": f"LIVE_AGENT_{child.upper()} retained tool result",
                            "reasoning": f"Inspecting {child} evidence while the turn remains active.",
                        }
                    )
                    live[child].set()
                    release[child].wait(90)
                    chunk({"content": f" FINISHED_AGENT_{child.upper()}"})
                    chunk({}, "stop")
                elif "PHASE2_NAV_START" in joined and not tooltail:
                    calls = [
                        tool(
                            "agent",
                            {
                                "label": f"{name} Worker",
                                "prompt": (
                                    f"PHASE2_CHILD_{name.upper()}: read activity-fixture.txt, "
                                    "report its exact content, then remain available."
                                ),
                                "provider": "native",
                                "mode": "continuable",
                            },
                            index=index,
                            identity=f"agent-{name.lower()}",
                        )
                        for index, name in enumerate(agents)
                    ]
                    chunk({"tool_calls": calls})
                    chunk({}, "tool_calls")
                else:
                    chunk({"content": "PARENT_NAV_READY"})
                    chunk({}, "stop")
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    os.environ["HEYCODE_PHASE2_NAV_FIXTURE"] = "local-only-not-a-real-credential"
    captures: list[str] = []
    assertions: list[str] = []
    single_agent_card: str | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-nav-home-") as home:
            with tempfile.TemporaryDirectory(prefix="heycode-nav-work-") as workspace:
                Path(workspace, "activity-fixture.txt").write_text(
                    "ACTUAL_PHASE2_CHILD_READ_RESULT\n"
                )
                base = f"http://127.0.0.1:{server.server_port}/api/v1"
                Path(home, "settings.toml").write_text(
                    f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n'
                )
                Path(home, "config.toml").write_text(
                    "schema_version = 30\n"
                    "[llm]\n"
                    'provider = "openrouter"\n'
                    f'model = "{MODEL["id"]}"\n'
                    'api_key_env = "HEYCODE_PHASE2_NAV_FIXTURE"\n'
                    f'base_url = "{base}"\n'
                )
                tui = FullScreenTui(
                    home,
                    workspace,
                    str(binary),
                    fake=False,
                    rows=rows,
                    columns=columns,
                    color=color,
                    extra=[
                        "--provider",
                        "openrouter",
                        "--model",
                        MODEL["id"],
                        "--approval",
                        "full_access",
                        "--set",
                        f"llm.base_url={base}",
                        "--set",
                        "llm.api_key_env=HEYCODE_PHASE2_NAV_FIXTURE",
                    ],
                )
                screen = Screen(columns, rows)
                stream = pyte.ByteStream(screen)

                def read(seconds: float = 0.1) -> str:
                    stream.feed(tui.read(seconds))
                    return "\n".join(screen.display)

                def send(data: bytes) -> None:
                    os.write(tui.fd, data)

                def wait_for(needle: str, timeout: float = 30) -> str:
                    deadline = time.monotonic() + timeout
                    latest = ""
                    while time.monotonic() < deadline:
                        latest = read()
                        if needle in latest:
                            return latest
                        if not tui.alive():
                            raise AssertionError(
                                f"CLI exited while waiting for {needle}:\n{latest}"
                            )
                    raise AssertionError(f"Missing {needle}:\n{latest}")

                def resize(next_columns: int, next_rows: int) -> str:
                    screen.resize(next_rows, next_columns)
                    fcntl.ioctl(
                        tui.fd,
                        termios.TIOCSWINSZ,
                        struct.pack("HHHH", next_rows, next_columns, 0, 0),
                    )
                    return read(0.4)

                def capture(name: str) -> str:
                    visible = read(0.25)
                    (output / f"{name}.txt").write_text(visible)
                    (output / f"{name}.ansi").write_bytes(tui.transcript)
                    render_screen(
                        screen,
                        output / f"{name}.png",
                        background="#f8f9fb" if theme == "heycode-light" else "#101014",
                        foreground="#20242c" if theme == "heycode-light" else "#e8eaf0",
                    )
                    captures.append(name)
                    return visible

                def click_line(pattern: str) -> str:
                    read(0.15)
                    for y, line in enumerate(screen.display):
                        match = re.search(pattern, line)
                        if match:
                            x = match.start() + 1
                            send(f"\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m".encode())
                            return read(0.3)
                    raise AssertionError(
                        f"No clickable line matching {pattern}:\n" + "\n".join(screen.display)
                    )

                try:
                    initial = read(2)
                    if "Welcome to heycode" in initial:
                        send(b"\x1b[B\x1b[B\r")
                        wait_for("Select a provider")
                        send(b"OpenRouter\r")
                        wait_for("Paste your OpenRouter API key")
                        send(b"local-only-not-a-real-credential\r")
                        wait_for("Choose a model")
                        send(b"\r")
                    wait_for("full access on")
                    send(b"PHASE2_NAV_START\r")
                    wait_for("PARENT_NAV_READY", 45)
                    assert all(event.wait(8) for event in live.values())
                    if cards_only:
                        main = capture("01-single-agent-live")
                        assert "Atlas Worker" in main and "running" in main, main
                        single_agent_card = "Agent(label)" if "Agent(Atlas Worker)" in main else "count tree"
                        if single_agent_card == "Agent(label)":
                            assert "Backgrounded agent · running" in main, main
                        else:
                            assert "● 1 agent" in main, main
                        assertions.append("one native child remains live with authoritative running status")

                        # The child owns a durable session and must actually settle;
                        # provider stream completion alone is not runtime settlement.
                        release["Atlas"].set()
                        deadline = time.monotonic() + 30
                        child_events = None
                        while time.monotonic() < deadline:
                            read()
                            for path in Path(home).rglob("session.jsonl"):
                                try:
                                    events = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
                                except json.JSONDecodeError:
                                    continue
                                if any("PHASE2_CHILD_ATLAS" in json.dumps(event) and event.get("kind") == "user/message" for event in events):
                                    if any(event.get("kind") == "turn/end" and event.get("data", {}).get("reason") == "stop" for event in events):
                                        child_events = events
                                        break
                            if child_events is not None:
                                break
                        assert child_events is not None, "native child did not retain successful turn/end"
                        (output / "child-events.json").write_text(json.dumps(child_events, indent=2))
                        wait_for("idle")
                        if single_agent_card == "Agent(label)":
                            wait_for('Agent "Atlas Worker" finished')
                            assertions.append("uniquely attributed native job completion shows the child name")
                        collapsed = capture("02-single-agent-settled")
                        assert "Atlas Worker" in collapsed and "idle" in collapsed, collapsed
                        assertions.append("continuable child retains successful turn/end and renders idle")

                        # The header expands metadata; the receipt opens this exact child.
                        click_line(r"(?:Agent\(Atlas Worker\)|● 1 agent)")
                        expanded = wait_for("background task admitted")
                        assert "Atlas Worker" in expanded and "task_id:" in expanded and "job_id:" in expanded, expanded
                        capture("03-single-agent-expanded")
                        click_line(r"[Aa]gent\(")
                        wait_for("idle")
                        click_line(r"(?:Backgrounded agent|└─ Atlas Worker)")
                        child = wait_for("@Atlas Worker (idle)")
                        assert "Progress" in child and "Read(activity-fixture.txt)" in child, child
                        assert "Prompt" in child and "PHASE2_CHILD_ATLAS" in child, child
                        capture("04-single-agent-retained-child")
                        assertions.append("card expands retained metadata and receipt opens the settled child's inspector")
                    else:
                        main = capture("01-main-five-agents")
                        assert main.count(" Worker") >= 5, main
                        assert "● main" in main and "↓" in main, main
                        assertions.append("five live native children render under main")

                        # Direct keyboard handoff: composer -> main -> first child.
                        send(b"\x1b[B")
                        read(0.2)
                        send(b"\x1b[B")
                        focused = capture("02-keyboard-focus-first-agent")
                        assert "› ○ Atlas Worker" in focused, focused
                        send(b"\r")
                        first = wait_for("LIVE_AGENT_ATLAS")
                        assert "● Atlas Worker" in first, first
                        click_line(r"Read 1 file")
                        expanded = wait_for("ACTUAL_PHASE2_CHILD_READ_RESULT")
                        assert "LIVE_AGENT_ATLAS" in expanded, expanded
                        capture("03-agent-tool-result")
                        assertions.append("keyboard opens child with retained live tool result")

                        click_line(r"[●○] main")
                        wait_for("PARENT_NAV_READY")
                        send(b"PARENT_NAV_DRAFT")
                        click_line(r"[●○] Equinox Worker")
                        wait_for("LIVE_AGENT_EQUINOX")
                        send(b"CHILD_EQUINOX_DRAFT")
                        child = capture("04-child-draft-and-switcher")
                        assert "CHILD_EQUINOX_DRAFT" in child and "● Equinox Worker" in child
                        click_line(r"[●○] main")
                        parent = wait_for("PARENT_NAV_DRAFT")
                        assert "CHILD_EQUINOX_DRAFT" not in parent
                        capture("05-parent-draft-restored")
                        click_line(r"[●○] Equinox Worker")
                        restored = wait_for("CHILD_EQUINOX_DRAFT")
                        assert "PARENT_NAV_DRAFT" not in restored
                        assertions.append("mouse switching restores isolated parent and child drafts")

                        narrow = resize(52, 30)
                        assert "Equinox Worker" in narrow and "main" in narrow, narrow
                        capture("06-narrow-active-agent")
                        resize(columns, rows)
                        assertions.append("active row remains reachable after narrow resize")

                        send(b"\x14")
                        listing = wait_for("Background")
                        assert "Agents (5)" in listing and "@team-lead" in listing, listing
                        capture("07-background-browser")
                        click_line(r"@Atlas Worker")
                        inspector = wait_for("Progress")
                        assert "Prompt" in inspector, inspector
                        assert "Read(activity-fixture.txt)" in inspector, inspector
                        assert "PHASE2_CHILD_ATLAS" in inspector, inspector
                        capture("08-agent-inspector")
                        send(b"\x1b")
                        restored = wait_for("CHILD_EQUINOX_DRAFT")
                        assert "PARENT_NAV_DRAFT" not in restored
                        assertions.append("inspector exposes Progress and Prompt without stealing owner")

                    if not color:
                        raw = bytes(tui.transcript)
                        sgr_parameters = {
                            parameter
                            for encoded in re.findall(rb"\x1b\[([0-9;]*)m", raw)
                            for parameter in encoded.split(b";")
                            if parameter
                        }
                        color_parameters = {
                            parameter
                            for parameter in sgr_parameters
                            if parameter in {b"38", b"48"}
                            or 30 <= int(parameter) <= 37
                            or 40 <= int(parameter) <= 47
                            or 90 <= int(parameter) <= 97
                            or 100 <= int(parameter) <= 107
                        }
                        assert not color_parameters, (
                            "NO_COLOR run emitted foreground/background SGR: "
                            f"{sorted(color_parameters)}"
                        )
                        assertions.append("NO_COLOR emitted no color SGR")

                    result = {
                        "passed": True,
                        "capture_started_utc": capture_started_utc,
                        "capture_finished_utc": datetime.now(timezone.utc).isoformat(),
                        "cards_only": cards_only,
                        "single_agent_card": single_agent_card,
                        "theme": theme,
                        "color": color,
                        "screens": captures,
                        "assertions": assertions,
                        "provider_requests": len(requests),
                        "binary": str(binary),
                        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                        "runtime": "native",
                        "transport": "localhost deterministic SSE fixture",
                        "external_provider": False,
                    }
                    (output / "result.json").write_text(json.dumps(result, indent=2))
                    print(json.dumps(result, indent=2))
                    return result
                except Exception:
                    capture("failure")
                    raise
                finally:
                    for event in release.values():
                        event.set()
                    tui.close()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    parser.add_argument("--color", action="store_true")
    parser.add_argument("--cards-only", action="store_true", help="Capture a single live and settled native agent card")
    parser.add_argument("--columns", type=int, default=100)
    parser.add_argument("--rows", type=int, default=45)
    args = parser.parse_args()
    run(
        args.binary.resolve(),
        args.output.resolve(),
        theme=args.theme,
        color=args.color,
        columns=args.columns,
        rows=args.rows,
        cards_only=args.cards_only,
    )
