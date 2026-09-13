#!/usr/bin/env python3
"""Exercise focused `/compact` through the real offline heycode TUI and restart.

The journey uses an immutable caller-supplied binary, a disposable HEYCODE_HOME
and workspace, and the built-in fake provider. It makes no paid or external
provider request. Rust integration tests separately inspect the exact focused
provider payload; this black-box check proves the terminal command, durable
journal, continuation, and reopen path are composed together.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time
from tempfile import TemporaryDirectory

import pyte

from terminal_screenshot import render_screen
from tui_blackbox import FullScreenTui


PROMPTS = [
    "ALPHA decision: retain the transport abstraction.",
    "BETA file: crates/example/src/lib.rs remains unfinished.",
    "GAMMA constraint: cancellation must leave the journal unchanged.",
]
FOCUS = "Prioritize API decisions, cancellation, and exact filenames."


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(binary: Path, output: Path) -> None:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    with TemporaryDirectory(prefix="heycode-compact-focus-") as temporary:
        root = Path(temporary)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        tui: FullScreenTui | None = None
        screen: Screen | None = None
        stream: pyte.ByteStream | None = None
        transcripts: list[bytes] = []
        captures: list[str] = []

        def start(extra: list[str] | None = None) -> None:
            nonlocal tui, screen, stream
            tui = FullScreenTui(
                str(home),
                str(workspace),
                str(binary),
                fake=True,
                color=True,
                rows=50,
                columns=110,
                extra=extra or [],
            )
            screen = Screen(110, 50)
            stream = pyte.ByteStream(screen)

        def read(seconds: float = 0.2) -> None:
            assert tui is not None and stream is not None
            stream.feed(tui.read(seconds))

        def visible() -> str:
            assert screen is not None
            return "\n".join(screen.display)

        def wait_for(predicate, label: str, timeout: float = 30) -> None:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                read()
                if predicate():
                    return
                assert tui is not None
                if not tui.alive():
                    raise AssertionError(f"heycode exited while waiting for {label}")
            raise AssertionError(f"timed out waiting for {label}\n{visible()}")

        def send(value: str) -> None:
            assert tui is not None
            os.write(tui.fd, value.encode() + b"\r")
            read()

        def log_path() -> Path:
            logs = list(home.rglob("session.jsonl"))
            assert len(logs) == 1, logs
            return logs[0]

        def events() -> list[dict]:
            path = log_path()
            return [json.loads(line) for line in path.read_text().splitlines() if line]

        def count(kind: str) -> int:
            return sum(event["kind"] == kind for event in events())

        def capture(name: str) -> None:
            read(0.4)
            text = visible()
            (output / f"{name}.txt").write_text(text)
            assert screen is not None
            render_screen(screen, output / f"{name}.png")
            captures.append(name)

        def stop() -> None:
            nonlocal tui
            if tui is None:
                return
            if tui.alive():
                os.write(tui.fd, b"/quit\r")
                deadline = time.monotonic() + 5
                while tui.alive() and time.monotonic() < deadline:
                    read()
            transcripts.append(tui.transcript)
            tui.close()
            tui = None

        try:
            start()
            wait_for(
                lambda: tui is not None and b"\x1b[?2004h" in tui.transcript,
                "terminal readiness",
            )
            capture("00-start")

            for index, prompt in enumerate(PROMPTS, 1):
                send(prompt)
                wait_for(lambda index=index: count("turn/end") == index, f"turn {index}")
                capture(f"0{index}-turn-{index}")

            before_list = log_path().read_bytes()
            send("/compact list")
            wait_for(
                lambda: all(
                    item in visible()
                    for item in ("portable-summary", "provider-native", "prune-oldest")
                ),
                "strategy list",
            )
            assert log_path().read_bytes() == before_list
            capture("04-strategy-list")

            pre_compaction = log_path().read_bytes()
            send(f"/compact {FOCUS}")
            wait_for(lambda: count("compaction/applied") == 1, "focused compaction")
            after_compaction = log_path().read_bytes()
            assert after_compaction.startswith(pre_compaction)
            capture("05-focused-compact")

            send("CONTINUATION-AFTER-FOCUSED-COMPACT")
            wait_for(lambda: count("turn/end") == 4, "post-compact continuation")
            capture("06-continuation")
            path = log_path()
            stop()

            compactions_before_restart = count_from_path(path, "compaction/applied")
            start(["--resume", str(path)])
            wait_for(
                lambda: tui is not None and b"\x1b[?2004h" in tui.transcript,
                "resumed terminal readiness",
            )
            assert count("compaction/applied") == compactions_before_restart
            capture("07-restarted")
            send("RESTART-CONTINUATION-AFTER-FOCUSED-COMPACT")
            wait_for(lambda: count("turn/end") == 5, "restart continuation")
            capture("08-restart-continuation")

            final_bytes = log_path().read_bytes()
            final_events = events()
            assert final_bytes.startswith(pre_compaction)
            assert count("compaction/applied") == 1
            user_text = [
                event["data"]["text"]
                for event in final_events
                if event["kind"] == "user/message"
            ]
            for prompt in [
                *PROMPTS,
                "CONTINUATION-AFTER-FOCUSED-COMPACT",
                "RESTART-CONTINUATION-AFTER-FOCUSED-COMPACT",
            ]:
                assert user_text.count(prompt) == 1, (prompt, user_text)
            compaction = next(
                event for event in final_events if event["kind"] == "compaction/applied"
            )
            assert "FAKE-REPLY" in compaction["data"]["summary"]
            headers = [
                event["data"]["header"]
                for event in final_events
                if event["kind"] == "request/header"
            ]
            # The sanctioned fake provider is the legacy in-process smoke
            # adapter, so it intentionally emits no transport request header.
            # A header here would mean this supposedly offline journey took a
            # different dispatch route.
            assert not headers, headers

            (output / "events.json").write_text(json.dumps(final_events, indent=2))
            (output / "session.jsonl").write_bytes(final_bytes)
            result = {
                "status": "passed",
                "binary": str(binary),
                "binary_sha256": sha256(binary),
                "captures": captures,
                "provider": "fake",
                "external_provider_requests": 0,
                "durable_transport_request_headers": len(headers),
                "original_prompts_retained": len(PROMPTS),
                "compaction_events": count("compaction/applied"),
                "continuation_before_restart": True,
                "continuation_after_restart": True,
                "journal_prefix_byte_exact": final_bytes.startswith(pre_compaction),
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
        finally:
            stop()
            (output / "terminal.ansi").write_bytes(b"\n".join(transcripts))

    print(f"PASS: {output}")


def count_from_path(path: Path, kind: str) -> int:
    return sum(
        json.loads(line)["kind"] == kind
        for line in path.read_text().splitlines()
        if line
    )


def sha256(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    run(arguments.binary, arguments.output)
