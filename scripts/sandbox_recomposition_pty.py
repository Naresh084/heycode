#!/usr/bin/env python3
"""Verify sandbox selection reopens the same native session without model traffic."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
import time
from pathlib import Path

from command_reference_pty import Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


def run(binary: Path, sha256: str, output: Path) -> None:
    binary = binary.resolve()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == sha256
    output.mkdir(parents=True, exist_ok=False)
    captures: list[str] = []
    result = {"binary": str(binary), "sha256": sha256, "commercial_calls": 0,
              "provider": "offline fake", "model_prompt_sent": False, "viewport": [110, 42]}
    with tempfile.TemporaryDirectory(prefix="sandbox-recomposition-") as directory:
        root = Path(directory)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        tui = FullScreenTui(str(home), str(work), str(binary), color=True,
                            extra=("--sandbox", "off", "--approval", "full_access",
                                   "--set", "llm.max_output_tokens=321"), rows=42)
        screen = Screen(110, 42)
        stream = TerminalByteStream(screen)

        def read() -> str:
            stream.feed(tui.read(0.15))
            return "\n".join(screen.display)

        def wait(predicate, label: str, timeout: float = 25) -> str:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                text = read()
                if predicate(text):
                    return text
                if not tui.alive():
                    raise AssertionError(f"CLI exited while {label}")
            raise AssertionError(f"Timed out while {label}\n{read()}")

        def send(data: bytes) -> None:
            os.write(tui.fd, data)
            read()

        def command(value: str) -> None:
            send(value.encode() + b"\r")

        def capture(name: str) -> None:
            (output / f"{name}.txt").write_text(read())
            render_screen(screen, output / f"{name}.png")
            captures.append(name)

        def records() -> dict:
            return {path.parent.name: [json.loads(line) for line in path.read_text().splitlines() if line]
                    for path in home.rglob("session.jsonl")}

        def choose(mode: str, steps: bytes) -> None:
            command("/sandbox")
            wait(lambda text: "Configure mode" in text, "opening sandbox mode picker")
            capture(f"{mode}-before-selection")
            offset = len(tui.transcript)
            send(steps + b"\r")
            wait(lambda text: b"\x1b[?1049h" in tui.transcript[offset:]
                 and f"Sandbox mode set to: {mode}" in text, f"reopening with {mode}")
            assert set(records()) == {session_id}, "sandbox selection changed session identity"
            assert "Sandbox generation" in read(), "session title was lost"
            assert "full access on" in read(), "sandbox selection changed the approval policy"
            capture(f"{mode}-receipt")
            command("/sandbox")
            wait(lambda text: "Configure mode" in text, "reopening effective mode report")
            active = {"readonly": "Read-only sandbox", "workspace": "Workspace-write sandbox", "off": "No Sandbox"}[mode]
            assert any(active in line and "✔" in line for line in screen.display), read()
            capture(f"{mode}-effective")
            send(b"\x1b[C\x1b[C")
            wait(lambda text: f"sandbox.mode = {mode}" in text, "checking effective backend configuration")
            if mode != "off":
                assert "Active backend: none" not in read(), "confinement mode lacks an active backend"
            capture(f"{mode}-configuration")
            send(b"\x1b")

        try:
            wait(lambda text: "for shortcuts" in text or "shift+tab to cycle" in text, "startup")
            command("/name Sandbox generation")
            wait(lambda text: "Renamed to Sandbox generation" in text, "naming fixture session")
            session_id = next(iter(records()))
            capture("startup")
            choose("readonly", b"\x1b[A")
            choose("workspace", b"\x1b[A")
            choose("off", b"\x1b[B\x1b[B")
            journal = records()
            assert set(journal) == {session_id}
            events = journal[session_id]
            assert not any(row.get("kind") in {"user/message", "request/header", "turn/start"} for row in events)
            result.update(status="passed", session_id=session_id, durable_sessions=1,
                          model_requests=0, effective_modes_verified=["readonly", "workspace", "off"])
        except Exception as error:
            result.update(status="failed", failure=f"{type(error).__name__}: {error}")
            capture("failure")
        finally:
            (output / "journals.json").write_text(json.dumps(records(), indent=2))
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
            result.update(captures=captures, owned_process_alive=tui.alive())
            (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
    if result["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    run(args.binary, args.sha256, args.output)
