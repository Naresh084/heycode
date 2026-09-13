#!/usr/bin/env python3
"""Capture paired non-empty Stats surfaces without contacting a provider.

Claude reads a synthetic version-five ``stats-cache.json`` from a disposable
configuration root. heycode first creates two real local sessions through its
built-in fake provider, then enriches those disposable logs with deterministic
provider-reported usage/cache facts before a fresh process opens ``/stats``.
No user configuration, credentials, session history, or network provider is
read by either captured process.
"""
from __future__ import annotations

import argparse
from datetime import UTC, datetime, timedelta
import fcntl
import hashlib
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

from PIL import Image, ImageDraw
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
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
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

    def send(self, data: bytes, *, seconds: float = 0.2) -> str:
        os.write(self.master, data)
        return self.read(seconds)

    def capture(self, output: Path, name: str) -> str:
        visible = self.read(0.6)
        (output / f"{name}.txt").write_text(visible)
        (output / f"{name}.ansi").write_bytes(bytes(self.transcript))
        render_screen(self.screen, output / f"{name}.png")
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


def scrub_credentials(environment: dict[str, str]) -> None:
    for key in list(environment):
        if (
            key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_"))
            or key.endswith("_API_KEY")
            or key.endswith("_AUTH_TOKEN")
        ):
            environment.pop(key)


def claude_cache(now: datetime) -> dict[str, object]:
    days = [(now - timedelta(days=offset)).date().isoformat() for offset in (2, 1, 0)]
    model = "claude-opus-4-6"
    token_counts = [2_400, 4_800, 7_200]
    return {
        "version": 5,
        "lastComputedDate": days[-1],
        "dailyActivity": [
            {
                "date": day,
                "messageCount": messages,
                "sessionCount": sessions,
                "toolCallCount": tools,
            }
            for day, messages, sessions, tools in zip(
                days, (8, 13, 21), (1, 2, 3), (2, 4, 7), strict=True
            )
        ],
        "dailyModelTokens": [
            {"date": day, "tokensByModel": {model: tokens}}
            for day, tokens in zip(days, token_counts, strict=True)
        ],
        "dailyModelTokensVersion": 5,
        "modelUsage": {
            model: {
                "inputTokens": 10_500,
                "outputTokens": 3_900,
                "cacheReadInputTokens": 2_000,
                "cacheCreationInputTokens": 750,
                "webSearchRequests": 0,
                "costUSD": 0,
                "contextWindow": 200_000,
                "maxOutputTokens": 64_000,
            }
        },
        "totalSessions": 6,
        "totalMessages": 42,
        "longestSession": {
            "sessionId": "00000000-0000-4000-8000-000000000001",
            "timestamp": f"{days[1]}T09:00:00.000Z",
            "duration": 3_600_000,
            "messageCount": 13,
        },
        "firstSessionDate": f"{days[0]}T08:00:00.000Z",
        "hourCounts": {"9": 5, "14": 12, "20": 3},
    }


def write_claude_transcript_fixture(config: Path, workspace: Path, now: datetime) -> Path:
    """Give Claude's Stats owner one discoverable transcript without inference."""
    session_id = "00000000-0000-4000-8000-000000000002"
    timestamp = (now - timedelta(days=1)).replace(
        hour=9, minute=0, second=0, microsecond=0
    ).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    project = config / "projects" / "-local-stats-fixture"
    project.mkdir(parents=True)
    user_id = "00000000-0000-4000-8000-000000000003"
    assistant_id = "00000000-0000-4000-8000-000000000004"
    common = {
        "isSidechain": False,
        "userType": "external",
        "cwd": str(workspace),
        "sessionId": session_id,
        "version": "2.1.268",
        "gitBranch": "",
        "timestamp": timestamp,
        "fixtureSeeded": True,
    }
    events = [
        {
            **common,
            "parentUuid": None,
            "type": "user",
            "message": {"role": "user", "content": "LOCAL_STATS_FIXTURE"},
            "uuid": user_id,
        },
        {
            **common,
            "parentUuid": user_id,
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-6",
                "id": "msg_local_stats_fixture",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "LOCAL_STATS_FIXTURE_REPLY"}],
                "stop_reason": "end_turn",
                "stop_sequence": None,
                "usage": {
                    "input_tokens": 10,
                    "cache_creation_input_tokens": 2,
                    "cache_read_input_tokens": 3,
                    "output_tokens": 4,
                },
            },
            "uuid": assistant_id,
            "requestId": "req_local_stats_fixture",
        },
    ]
    path = project / f"{session_id}.jsonl"
    path.write_text(
        "".join(json.dumps(event, separators=(",", ":")) + "\n" for event in events)
    )
    return path


def run_claude(
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    executable_value = shutil.which("claude")
    if executable_value is None:
        raise RuntimeError("Claude CLI is not installed on PATH")
    executable = Path(executable_value).resolve()
    output.mkdir()
    with tempfile.TemporaryDirectory(prefix="heycode-stats-claude-") as folder:
        root = Path(folder)
        sandbox_home = root / "home"
        config = root / "config"
        workspace = root / "workspace"
        for path in (sandbox_home, config, workspace):
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
        now = datetime.now(UTC)
        cache = claude_cache(now)
        (config / "stats-cache.json").write_text(json.dumps(cache, indent=2) + "\n")
        transcript_fixture = write_claude_transcript_fixture(config, workspace, now)
        environment = os.environ.copy()
        scrub_credentials(environment)
        environment.update(
            {
                "HOME": str(sandbox_home),
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
            }
        )
        session_id = str(uuid.uuid4())
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
                session_id,
                "--name",
                "terminal-stats-cache-reference",
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
            initial = terminal.read(3)
            for _ in range(4):
                lower = initial.lower()
                if "custom api key" in lower and "do you want to use" in lower:
                    initial = terminal.send(b"\x1b[A\r", seconds=1.5)
                    continue
                if "trust" in lower and "folder" in lower:
                    initial = terminal.send(b"\x1b[B\r", seconds=1.5)
                    continue
                break
            terminal.wait("shift+tab to cycle", "Claude Code")
            terminal.send(b"/stats\r")
            stats = terminal.wait("Favorite model", "Longest session", "Current streak")
            if "Stats" not in stats:
                raise AssertionError(stats)
            visible = terminal.capture(output, "claude-stats-nonempty")
            for expected in ("Favorite model", "Longest session", "Current streak"):
                if expected not in visible:
                    raise AssertionError(f"Claude Stats omitted {expected!r}:\n{visible}")
            (output / "stats-cache.fixture.json").write_text(json.dumps(cache, indent=2) + "\n")
            shutil.copyfile(transcript_fixture, output / "transcript.fixture.jsonl")
            (output / "terminal.ansi").write_bytes(bytes(terminal.transcript))
        finally:
            terminal.close()
    return {
        "status": "passed",
        "runtime": "Claude Code full-screen TUI over PTY",
        "version": subprocess.run(
            [str(executable), "--version"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip(),
        "binary": str(executable),
        "binary_sha256": sha256(executable),
        "source": "synthetic version-five stats-cache.json in disposable CLAUDE_CONFIG_DIR",
        "conversation_prompts": 0,
        "external_provider_requests": 0,
        "capture": "claude-stats-nonempty.png",
    }


def heycode_environment(home: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HEYCODE_HOME": str(home),
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
        }
    )
    environment.pop("NO_COLOR", None)
    return environment


def create_heycode_session(
    binary: Path,
    home: Path,
    workspace: Path,
    prompt: str,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> None:
    terminal = Terminal(
        [str(binary), "--fake", "--no-background", "--trust-workspace"],
        cwd=workspace,
        environment=heycode_environment(home),
        columns=columns,
        rows=rows,
        timeout=timeout,
    )
    try:
        terminal.wait("shift+tab to cycle")
        terminal.send(prompt.encode())
        terminal.send(b"\r")
        terminal.wait("FAKE-REPLY: offline smoke response.")
        terminal.send(b"/quit\r")
        deadline = time.monotonic() + 5
        while terminal.process.poll() is None and time.monotonic() < deadline:
            terminal.read(0.1)
    finally:
        terminal.close()


def enrich_heycode_session(log: Path, *, prompt_tokens: int, completion_tokens: int) -> None:
    events = [json.loads(line) for line in log.read_text().splitlines() if line.strip()]
    headers = [event for event in events if event.get("kind") == "request/header"]
    assistants = [event for event in events if event.get("kind") == "assistant/message"]
    if headers or len(assistants) != 1:
        raise AssertionError((log, len(headers), len(assistants)))
    assistant = assistants[0]
    step_starts = [
        event
        for event in events
        if event.get("kind") == "step/start"
        and event["data"]["turn"] == assistant["data"]["turn"]
        and event["data"]["step"] == assistant["data"]["step"]
    ]
    if len(step_starts) != 1:
        raise AssertionError((log, "step/start", len(step_starts)))
    request_id = str(uuid.uuid4())
    request = {
        "v": 2,
        "seq": 0,
        "time_ms": step_starts[0]["time_ms"],
        "kind": "request/header",
        "data": {
            "turn": assistant["data"]["turn"],
            "step": assistant["data"]["step"],
            "request_id": request_id,
            "header": {
                "provider": "local-fixture",
                "model": "stats-fixture-model",
                "protocol": "open_ai_chat_completions",
                "target": {"kind": "http", "base_url": "http://127.0.0.1:9"},
                "authentication": {"kind": "none"},
                "prompt_sha256": hashlib.sha256(b"").hexdigest(),
                "tools": [],
                "options": {
                    "input_modalities": ["text"],
                    "defaulted_reasoning_effort": False,
                    "native_features": [],
                    "native_tool_routes": [],
                    "provider_options": [],
                    "defaulted_max_output_tokens": False,
                    "purpose": "conversation",
                    "retry": {"max_attempts": 1, "safety": "never"},
                },
            },
        },
    }
    events.insert(events.index(step_starts[0]) + 1, request)
    assistant["data"]["usage"] = {
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
    }
    insertion = events.index(assistant)
    metadata = {
        "v": 2,
        "seq": assistant["seq"],
        "time_ms": assistant["time_ms"],
        "kind": "assistant/response-metadata",
        "data": {
            "turn": assistant["data"]["turn"],
            "step": assistant["data"]["step"],
            "request_id": request_id,
            "metadata": {
                "schema_version": 1,
                "cache_usage": {
                    "schema_version": 1,
                    "input_tokens": prompt_tokens,
                    "output_tokens": completion_tokens,
                    "cache_read_tokens": min(200, prompt_tokens),
                    "cache_write_tokens": min(75, max(0, prompt_tokens - 200)),
                    "reasoning_tokens": 0,
                },
            },
        },
    }
    events.insert(insertion, metadata)
    for sequence, event in enumerate(events):
        event["seq"] = sequence
    log.write_text("".join(json.dumps(event, separators=(",", ":")) + "\n" for event in events))


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
    output.mkdir()
    with tempfile.TemporaryDirectory(prefix="heycode-stats-native-") as folder:
        root = Path(folder)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        (home / "config.toml").write_text(
            'schema_version = 31\n\n[ui]\naccent = "#224466"\n'
        )
        for ordinal in (1, 2):
            create_heycode_session(
                binary,
                home,
                workspace,
                f"LOCAL_STATS_FIXTURE_{ordinal}",
                columns=columns,
                rows=rows,
                timeout=timeout,
            )
        logs = sorted((home / "sessions").glob("*/session.jsonl"))
        used_logs = []
        for log in logs:
            kinds = {
                json.loads(line).get("kind")
                for line in log.read_text().splitlines()
                if line.strip()
            }
            if "user/message" in kinds:
                used_logs.append(log)
        if len(used_logs) != 2:
            raise AssertionError(f"expected two used local sessions, found {len(used_logs)}")
        enrich_heycode_session(used_logs[0], prompt_tokens=800, completion_tokens=200)
        enrich_heycode_session(used_logs[1], prompt_tokens=1_200, completion_tokens=300)
        (used_logs[0].parent / ".archived").touch()

        terminal = Terminal(
            [str(binary), "--fake", "--no-background", "--trust-workspace"],
            cwd=workspace,
            environment=heycode_environment(home),
            columns=columns,
            rows=rows,
            timeout=timeout,
        )
        try:
            terminal.wait("shift+tab to cycle")
            terminal.send(b"/stats\r")
            stats = terminal.wait("Favorite model:")
            visible = terminal.capture(output, "heycode-stats-nonempty")
            terminal.send(b"\x1b[B")
            terminal.send(b"\x1b[C")
            model_rows = terminal.wait("All local history")
            model_visible = terminal.capture(output, "heycode-stats-nonempty-models")
            combined = "\n".join((stats, visible, model_rows, model_visible))
            expected = (
                "Overview", "Models", "Sessions: 2", "Total tokens: 2.5k",
                "Input 2k · Output 500 · Cache read 400 · Cache write 150",
                "Favorite model:", "All local history", "active + archived",
            )
            for value in expected:
                if value not in combined:
                    raise AssertionError(f"heycode Stats omitted {value!r}:\n{combined}")
            (output / "terminal.ansi").write_bytes(bytes(terminal.transcript))
        finally:
            terminal.close()

        fixtures = output / "fixtures"
        fixtures.mkdir()
        event_counts: dict[str, int] = {}
        for log in used_logs:
            destination = fixtures / f"{log.parent.name}.jsonl"
            shutil.copyfile(log, destination)
            event_counts[log.parent.name] = sum(1 for _ in log.open())
        (fixtures / f"{used_logs[0].parent.name}.archived-marker").touch()

    return {
        "status": "passed",
        "runtime": "real heycode CLI full-screen TUI over PTY",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider": "built-in fake only",
        "source": "two CLI-created disposable durable logs with deterministic reported usage/cache facts",
        "used_sessions": 2,
        "archived_sessions": 1,
        "fixture_event_counts": event_counts,
        "external_provider_requests": 0,
        "captures": ["heycode-stats-nonempty.png", "heycode-stats-nonempty-models.png"],
    }


def paired_image(claude: Path, heycode: Path, output: Path) -> None:
    left = Image.open(claude).convert("RGB")
    right = Image.open(heycode).convert("RGB")
    if left.size != right.size:
        raise AssertionError(f"capture viewport mismatch: {left.size} != {right.size}")
    label_height = 36
    canvas = Image.new("RGB", (left.width + right.width, left.height + label_height), "#111827")
    canvas.paste(left, (0, label_height))
    canvas.paste(right, (left.width, label_height))
    draw = ImageDraw.Draw(canvas)
    draw.text((12, 10), "Claude Code source reference (synthetic cache)", fill="#f9fafb")
    draw.text((left.width + 12, 10), "heycode implementation (durable local projection)", fill="#f9fafb")
    canvas.save(output)


def run(
    binary: Path,
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, object]:
    output.mkdir(parents=True, exist_ok=False)
    claude = run_claude(output / "claude", columns=columns, rows=rows, timeout=timeout)
    heycode = run_heycode(binary, output / "heycode", columns=columns, rows=rows, timeout=timeout)
    paired_image(
        output / "claude" / "claude-stats-nonempty.png",
        output / "heycode" / "heycode-stats-nonempty.png",
        output / "paired-stats-nonempty.png",
    )
    result: dict[str, object] = {
        "status": "passed",
        "scope": "paired non-empty Stats terminal acceptance",
        "viewport": {"columns": columns, "rows": rows},
        "claude": claude,
        "heycode": heycode,
        "external_provider_requests": 0,
        "conversation_prompts_to_external_models": 0,
        "evidence_boundary": (
            "Controlled local terminal acceptance over synthetic non-empty data. "
            "It proves both immutable binaries render their Stats surfaces without "
            "a provider request; it does not prove account-level limits, billing, or "
            "live-provider usage reconciliation."
        ),
        "paired_capture": "paired-stats-nonempty.png",
    }
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=110)
    parser.add_argument("--rows", type=int, default=46)
    parser.add_argument("--timeout", type=float, default=30.0)
    arguments = parser.parse_args()
    try:
        result = run(
            arguments.binary,
            arguments.out,
            columns=arguments.columns,
            rows=arguments.rows,
            timeout=arguments.timeout,
        )
    except Exception as error:  # noqa: BLE001 - acceptance runner reports every blocker
        print(f"FAIL: {type(error).__name__}: {error}", file=os.sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
