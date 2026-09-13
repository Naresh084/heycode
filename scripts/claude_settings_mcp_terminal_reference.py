#!/usr/bin/env python3
"""Capture current Claude Code /config, /mcp, and /plugin reference surfaces.

The process receives disposable HOME, config, and workspace roots and runs in
restricted, strict-MCP mode. By default it also uses safe mode; the optional
configured-MCP journey instead uses bare mode and one disposable local stdio
fixture. Plugin captures run with network syscalls denied for Claude and every
child process. Only built-in slash commands and navigation keys are sent; no
conversation/model prompt is submitted.
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
import sys
import tempfile
import termios
import time
import uuid

import pyte

from terminal_screenshot import TerminalByteStream, render_screen


MCP_FIXTURE_SOURCE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

log_path = Path(sys.argv[1])
mode = sys.argv[2]

def record(value):
    with log_path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, separators=(",", ":")) + "\n")

record({"event": "start", "pid": os.getpid()})
if mode == "fail":
    record({"event": "intentional-failure"})
    raise SystemExit(23)
try:
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = message.get("method")
        record({"event": "message", "method": method, "id": message.get("id")})
        if "id" not in message:
            continue
        if method == "initialize":
            result = {
                "protocolVersion": message.get("params", {}).get(
                    "protocolVersion", "2024-11-05"
                ),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "terminal-claude-reference", "version": "1"},
            }
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": "local_reference_tool",
                        "description": "credential-free local reference tool",
                        "inputSchema": {"type": "object"},
                    }
                ]
            }
        elif method == "resources/list":
            result = {"resources": []}
        elif method == "prompts/list":
            result = {"prompts": []}
        elif method == "ping":
            result = {}
        else:
            result = {}
        print(
            json.dumps(
                {"jsonrpc": "2.0", "id": message["id"], "result": result},
                separators=(",", ":"),
            ),
            flush=True,
        )
finally:
    record({"event": "stop", "pid": os.getpid()})
'''


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def run(
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
    configured_mcp: bool,
    fixture_failure: bool,
    plugin_only: bool,
    configured_plugin: bool,
) -> dict[str, object]:
    executable = shutil.which("claude")
    if executable is None:
        raise RuntimeError("Claude Code is unavailable")
    output.mkdir(parents=True, exist_ok=False)
    session_id = str(uuid.uuid4())
    transcript = bytearray()
    captures: list[str] = []
    blocker: str | None = None
    command_inputs: list[str] = []

    with tempfile.TemporaryDirectory(prefix="claude-config-mcp-reference-") as folder:
        root = Path(folder)
        disposable_home = root / "home"
        config = root / "claude-config"
        workspace = root / "workspace"
        disposable_home.mkdir()
        config.mkdir()
        workspace.mkdir()
        if configured_plugin:
            marketplace = root / "terminal-marketplace"
            plugin = config / "plugins/cache/terminal-local/local-audit/1.0.0"
            source_plugin = marketplace / "plugins/local-audit"
            (marketplace / ".claude-plugin").mkdir(parents=True)
            (plugin / ".claude-plugin").mkdir(parents=True)
            (source_plugin / ".claude-plugin").mkdir(parents=True)
            (workspace / ".claude").mkdir()
            (marketplace / ".claude-plugin/marketplace.json").write_text(
                json.dumps(
                    {
                        "name": "terminal-local",
                        "owner": {"name": "Phase 2 local fixture"},
                        "plugins": [
                            {
                                "name": "local-audit",
                                "source": "./plugins/local-audit",
                                "description": "Local-only plugin UI reference fixture",
                                "version": "1.0.0",
                            }
                        ],
                    }
                )
            )
            plugin_manifest = json.dumps(
                {
                    "name": "local-audit",
                    "version": "1.0.0",
                    "description": "Local-only plugin UI reference fixture",
                    "author": {"name": "Phase 2 fixture"},
                }
            )
            (plugin / ".claude-plugin/plugin.json").write_text(plugin_manifest)
            (source_plugin / ".claude-plugin/plugin.json").write_text(plugin_manifest)
            plugins = config / "plugins"
            (plugins / "known_marketplaces.json").write_text(
                json.dumps(
                    {
                        "terminal-local": {
                            "source": {
                                "source": "directory",
                                "path": str(marketplace),
                            },
                            "installLocation": str(marketplace),
                            "lastUpdated": "2026-09-12T00:00:00.000Z",
                        }
                    }
                )
            )
            installed = {
                "version": 2,
                "plugins": {
                    "local-audit@terminal-local": [
                        {
                            "scope": "user",
                            "version": "1.0.0",
                            "installedAt": "2026-09-12T00:00:00.000Z",
                            "lastUpdated": "2026-09-12T00:00:00.000Z",
                            "installPath": str(plugin),
                            "gitCommitSha": "0123456789abcdef0123456789abcdef01234567",
                        }
                    ]
                },
            }
            (plugins / "installed_plugins.json").write_text(json.dumps(installed))
            settings = {
                "enabledPlugins": {"local-audit@terminal-local": True},
                "extraKnownMarketplaces": {
                    "terminal-local": {
                        "source": {
                            "source": "directory",
                            "path": str(marketplace),
                        }
                    }
                },
            }
            (config / "settings.json").write_text(json.dumps(settings))
            (output / "seeded-installed-plugins.json").write_text(
                json.dumps(installed, indent=2)
            )
            (output / "seeded-settings.json").write_text(json.dumps(settings, indent=2))
        fixture_log = root / "mcp-fixture.jsonl"
        mcp_config = root / "mcp-config.json"
        if configured_mcp:
            fixture = root / "mcp_fixture.py"
            fixture.write_text(MCP_FIXTURE_SOURCE)
            mcp_config.write_text(
                json.dumps(
                    {
                        "mcpServers": {
                            "fixture": {
                                "type": "stdio",
                                "command": sys.executable,
                                "args": [
                                    str(fixture),
                                    str(fixture_log),
                                    "fail" if fixture_failure else "ready",
                                ],
                            }
                        }
                    }
                )
            )
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
        environment.update(
            {
                "HOME": str(disposable_home),
                "CLAUDE_CONFIG_DIR": str(config),
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL": "1",
                "DISABLE_AUTOUPDATER": "1",
                "HTTP_PROXY": "http://127.0.0.1:9",
                "HTTPS_PROXY": "http://127.0.0.1:9",
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        environment.pop("NO_PROXY", None)
        environment.pop("NO_COLOR", None)
        bare_mode = configured_mcp or (plugin_only and not configured_plugin)
        safe_mode = not (configured_mcp or plugin_only)
        network_sandbox = plugin_only
        if network_sandbox:
            sandbox_exec = shutil.which("sandbox-exec")
            if sandbox_exec is None:
                raise RuntimeError("sandbox-exec is required for offline plugin capture")
            command = [
                sandbox_exec,
                "-p",
                "(version 1) (allow default) (deny network*)",
                executable,
            ]
        else:
            command = [executable]
        debug_log = root / "claude-debug.log"
        if configured_plugin:
            command.extend(
                [
                    "--debug-file",
                    str(debug_log),
                    # Restricted mode deliberately ignores ordinary user
                    # settings; this explicit file is the isolated fixture.
                    "--settings",
                    str(config / "settings.json"),
                ]
            )
        if configured_mcp:
            command.extend(["--bare", "--mcp-config", str(mcp_config)])
        elif plugin_only and not configured_plugin:
            command.append("--bare")
        elif not configured_plugin:
            command.append("--safe-mode")
        command.extend(
            [
                "--restricted",
                "--strict-mcp-config",
                "--no-chrome",
                "--permission-mode",
                "plan",
                "--setting-sources",
                "user,project,local" if configured_plugin else "project,local",
                "--session-id",
                session_id,
                "--name",
                "isolated-config-mcp-reference",
            ]
        )
        master, slave = pty.openpty()
        fcntl.ioctl(
            slave,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
        process = subprocess.Popen(
            command,
            cwd=workspace,
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)
        screen = Screen(columns, rows)
        stream = TerminalByteStream(screen)

        def visible() -> str:
            return "\n".join(screen.display)

        def read(seconds: float = 0.15) -> str:
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
            return visible()

        def send(data: bytes) -> None:
            os.write(master, data)
            read(0.25)

        def wait_for_any(needles: tuple[str, ...], seconds: float | None = None) -> str:
            deadline = time.monotonic() + (seconds or timeout)
            latest = visible()
            while time.monotonic() < deadline:
                latest = read()
                normalized = "".join(latest.split()).lower()
                if any("".join(needle.split()).lower() in normalized for needle in needles):
                    return latest
                if process.poll() is not None:
                    raise RuntimeError(
                        f"Claude exited with {process.returncode} waiting for {needles}:\n{latest}"
                    )
            raise TimeoutError(f"timed out waiting for {needles}:\n{latest}")

        def capture(name: str) -> str:
            text = read(0.5)
            (output / f"{name}.txt").write_text(text)
            (output / f"{name}.ansi").write_bytes(transcript)
            render_screen(screen, output / f"{name}.png")
            captures.append(name)
            return text

        def type_command(value: str) -> None:
            command_inputs.append(value)
            send(value.encode())

        try:
            initial = read(5.0)
            if "trust" in initial.lower() and "folder" in initial.lower():
                send(b"\x1b[B\r")
                initial = read(4.0)
            if any(
                marker in initial.lower()
                for marker in ("welcome to claude code", "select a theme")
            ):
                raise RuntimeError("isolated Claude configuration reached onboarding")
            wait_for_any(("shift+tab", "? for shortcuts", "try"), seconds=20)
            capture("00-start")

            if plugin_only:
                type_command("/plugin")
                wait_for_any(("Manage plugins", "plugin"), seconds=10)
                capture("01-plugin-command-menu")
                send(b"\r")
                plugin_panel = wait_for_any(
                    ("Plugins", "Discover", "Installed", "marketplace"), seconds=20
                )
                if "plugin" not in plugin_panel.lower():
                    raise AssertionError("/plugin did not expose a recognizable panel")
                capture("02-plugin-panel")
                send(b"\x1b[C")
                read(0.5)
                installed_panel = capture("03-plugin-next-tab")
                if configured_plugin:
                    if "local-audit" not in installed_panel:
                        raise AssertionError("seeded local plugin missing from Installed tab")
                    send(b"\r")
                    wait_for_any(("local-audit", "Version", "Enabled"), seconds=10)
                    capture("03b-plugin-installed-details")
                    send(b"\x1b[B")
                    read(0.4)
                    capture("03c-plugin-installed-action")
                send(b"\x1b")
                read(0.5)
                if configured_plugin:
                    # The first Escape leaves plugin details for Installed;
                    # the second leaves the manager for the composer.
                    send(b"\x1b")
                    read(0.5)
                type_command("/reload-plugins")
                wait_for_any(
                    ("Activate pending plugin changes", "reload-plugins"),
                    seconds=10,
                )
                capture("04-reload-plugins-command-menu")
                send(b"\r")
                read(3.0)
                if process.poll() is not None:
                    raise AssertionError("/reload-plugins exited the Claude process")
                capture("05-reload-plugins-result")
            else:
                type_command("/config")
                wait_for_any(("Open config panel", "config"), seconds=10)
                capture("01-config-command-menu")
                send(b"\r")
                config_panel = wait_for_any(("Settings", "Config"), seconds=20)
                if "settings" not in config_panel.lower() and "config" not in config_panel.lower():
                    raise AssertionError("/config did not expose a recognizable panel")
                capture("02-config-panel")
                send(b"\x1b[B")
                capture("03-config-keyboard-navigation")
                send(b"\x1b")
                read(0.5)

                type_command("/mcp")
                wait_for_any(("Manage MCP servers", "mcp"), seconds=10)
                capture("04-mcp-command-menu")
                send(b"\r")
                mcp_panel = wait_for_any(
                    ("Manage MCP servers", "No MCP servers configured", "MCP servers"),
                    seconds=20,
                )
                if "mcp" not in mcp_panel.lower():
                    raise AssertionError("/mcp did not expose a recognizable panel")
                capture("05-mcp-panel")
                if configured_mcp:
                    if "fixture" not in mcp_panel.lower():
                        raise AssertionError("configured MCP panel omitted local fixture")
                    send(b"\r")
                    details = wait_for_any(
                        ("fixture", "connected", "failed", "local_reference_tool"), seconds=10
                    )
                    if "fixture" not in details.lower():
                        raise AssertionError("configured MCP details omitted local fixture")
                    capture("06-mcp-configured-details")
                    if fixture_failure:
                        if "reconnect" not in details.lower():
                            raise AssertionError("failed MCP details omitted reconnect action")
                    else:
                        send(b"\x1b[B")
                        reconnect_selected = capture("07-mcp-reconnect-selected")
                        if "reconnect" not in reconnect_selected.lower():
                            raise AssertionError("configured MCP details omitted reconnect action")
                        send(b"\r")
                        wait_for_any(("connected", "reconnect"), seconds=10)
                        capture("08-mcp-reconnected")
        except Exception as error:
            blocker = f"{type(error).__name__}: {error}"
            capture("blocked")
        finally:
            (output / "terminal.ansi").write_bytes(transcript)
            if process.poll() is None:
                try:
                    process.send_signal(signal.SIGTERM)
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            os.close(master)

        created_files = sorted(
            str(path.relative_to(root))
            for path in root.rglob("*")
            if path.is_file()
        )
        fixture_events = []
        if fixture_log.is_file():
            fixture_events = [
                json.loads(line)
                for line in fixture_log.read_text(errors="replace").splitlines()
                if line.strip()
            ]
        if configured_plugin:
            for source, name in [
                (config / "plugins/installed_plugins.json", "final-installed-plugins.json"),
                (config / "plugins/known_marketplaces.json", "final-known-marketplaces.json"),
                (debug_log, "claude-debug.log"),
            ]:
                if source.is_file():
                    (output / name).write_bytes(source.read_bytes())

    version = subprocess.run(
        [executable, "--version"],
        check=False,
        capture_output=True,
        text=True,
    ).stdout.strip()
    result: dict[str, object] = {
        "engine": "Claude Code",
        "version": version,
        "status": "blocked" if blocker else "captured",
        "blocker": blocker,
        "workspace": "disposable temporary directory",
        "isolated_home_and_config": True,
        "safe_mode": safe_mode,
        "bare_mode": bare_mode,
        "network_sandbox": network_sandbox,
        "restricted_mode": True,
        "strict_mcp_config": True,
        "configured_local_mcp": configured_mcp,
        "fixture_failure": fixture_failure,
        "plugin_only": plugin_only,
        "configured_plugin": configured_plugin,
        "fixture_events": fixture_events,
        "command_inputs": command_inputs,
        "model_prompt_sent": False,
        "captures": captures,
        "created_files_under_disposable_root": len(created_files),
        "created_top_level_directories": sorted(
            {path.split("/", 1)[0] for path in created_files}
        ),
        "conversation_journal_files": [
            path
            for path in created_files
            if path.startswith("claude-config/projects/") and path.endswith(".jsonl")
        ],
    }
    (output / "result.json").write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=126)
    parser.add_argument("--rows", type=int, default=48)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument(
        "--configured-mcp",
        action="store_true",
        help="use one disposable credential-free local stdio MCP server",
    )
    parser.add_argument(
        "--configured-mcp-failure",
        action="store_true",
        help="make the configured local MCP fixture exit before initialization",
    )
    parser.add_argument(
        "--plugin-only",
        action="store_true",
        help="capture the built-in plugin manager without MCP/config actions",
    )
    parser.add_argument(
        "--configured-plugin",
        action="store_true",
        help="preseed one disposable local installed plugin for structural capture",
    )
    arguments = parser.parse_args()
    run(
        arguments.output.resolve(),
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
        configured_mcp=arguments.configured_mcp or arguments.configured_mcp_failure,
        fixture_failure=arguments.configured_mcp_failure,
        plugin_only=arguments.plugin_only or arguments.configured_plugin,
        configured_plugin=arguments.configured_plugin,
    )
