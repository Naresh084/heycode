#!/usr/bin/env python3
"""Prompt-free, isolated availability check of Claude's local /import surface."""
from __future__ import annotations
import argparse
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import threading
import time
import uuid

from workspace_transitions_pty import Terminal
from terminal_screenshot import TerminalByteStream


def exercise_cd(terminal, workspace: Path, result: dict):
    nested = workspace / "owned-nested"
    nested.mkdir()
    normalize = lambda text: " ".join(text.split())

    def submit(text: str):
        prompts = [line.strip() for line in terminal.visible().splitlines() if line.lstrip().startswith("❯")]
        if not prompts or prompts[-1] != "❯":
            raise RuntimeError("Composer is not empty; refusing to concatenate another command")
        os.write(terminal.master, text.encode())
        terminal.read(0.5)
        os.write(terminal.master, b"\r")

    # The exact local command has already been observed in the menu.
    os.write(terminal.master, b"\r")
    terminal.wait("Usage: /cd <path>")
    terminal.capture("02-bare-usage")
    submit("/cd .")
    terminal.wait(f"Already in {workspace}")
    terminal.capture("03-original-cwd")

    os.write(terminal.master, b"/cd owned-nested")
    terminal.read(0.5)
    terminal.capture("04-unsubmitted-path")
    os.write(terminal.master, b"\x1b")
    terminal.read(0.3)
    os.write(terminal.master, b"\x15")
    terminal.read(0.5)
    submit("/cd .")
    terminal.wait(f"Already in {workspace}")
    terminal.capture("05-cancelled-draft-cwd-unchanged")

    submit("/cd missing-owned-directory")
    terminal.wait(f"Couldn't find a directory at {workspace / 'missing-owned-directory'}")
    terminal.capture("06-missing-path-refused")
    submit("/cd owned-nested")
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        terminal.read(0.3)
        visible = normalize(terminal.visible())
        if f"Moved to {nested}" in visible or "Moving to a new directory:" in visible:
            break
    if "Moving to a new directory:" in normalize(terminal.visible()):
        terminal.capture("07-owned-directory-confirmation")
        os.write(terminal.master, b"\x1b")
        terminal.wait(f"Staying in {workspace}")
        terminal.capture("08-confirmation-cancelled")
        submit("/cd owned-nested")
        terminal.wait("Yes, move here")
        terminal.read(1)
        os.write(terminal.master, b"\x1b[B\r")
        result["owned_directory_confirmation"] = "cancelled_then_confirmed"
    else:
        result["owned_directory_confirmation"] = "already_trusted_no_dialog"
    terminal.wait(f"Moved to {nested}")
    terminal.capture("09-moved-to-owned-directory")
    submit("/cd .")
    terminal.wait(f"Already in {nested}")
    terminal.capture("10-real-current-directory")
    submit("/cd missing-after-move")
    terminal.wait(f"Couldn't find a directory at {nested / 'missing-after-move'}")
    terminal.capture("11-relative-refusal-after-move")
    submit("/cd ..")
    terminal.wait(f"Moved to {workspace}")
    terminal.capture("12-returned-to-original-directory")
    result.update({"status":"cd_bounded_journey_passed", "original_workspace":str(workspace),
                   "owned_subdirectory":str(nested), "checks":["bare command shows usage",
                   "unsubmitted path cancellation preserves cwd", "missing directory is refused",
                   "owned nested directory succeeds", "same-directory check proves actual new cwd",
                   "relative missing path resolves from new cwd", "parent command restores original cwd"]})


def run(output: Path, safe_mode: bool = True, command: str = "import", cd_journey: bool = False):
    if command not in ("import", "cd"):
        raise ValueError("Only the bounded import and cd reference queries are supported")
    output.mkdir(parents=True, exist_ok=False)
    executable = Path(shutil.which("claude") or "").resolve()
    if not executable.is_file():
        raise RuntimeError("Claude executable unavailable")
    requests = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def refuse(self):
            length = int(self.headers.get("Content-Length", "0"))
            if length:
                self.rfile.read(length)
            requests.append({"method": self.command, "path": self.path})
            body = b'{"error":{"message":"isolated import reference"}}'
            self.send_response(503)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        do_GET = refuse
        do_POST = refuse
        do_CONNECT = refuse
        def log_message(self, *_): pass
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    terminal = None
    result = {"engine":"Claude", "executable_version": executable.name, "command":f"/{command}", "safe_mode":safe_mode, "restricted":True, "commercial_requests":0, "prompt_submitted":False}
    try:
        with tempfile.TemporaryDirectory(prefix="claude-import-reference-") as folder:
            root = Path(folder).resolve()
            home, config, workspace = [root / name for name in ("home", "config", "workspace")]
            for path in (home, config, workspace): path.mkdir()
            (config / ".claude.json").write_text(json.dumps({"hasCompletedOnboarding":True,"theme":"dark","lastOnboardingVersion":"2.1.268"}))
            (home / ".codex").mkdir()
            (home / ".codex/AGENTS.md").write_text("ISOLATED_REFERENCE_GUIDANCE")
            environment = {key:value for key,value in os.environ.items() if not (
                key.endswith(("_API_KEY", "_AUTH_TOKEN", "_ACCESS_TOKEN")) or key.startswith(("ANTHROPIC_", "CLAUDE_CODE_OAUTH", "CLAUDE_CODE_USE_")))}
            loopback = f"http://127.0.0.1:{server.server_port}"
            environment.update({"HOME":str(home),"CLAUDE_CONFIG_DIR":str(config),"TERM":"xterm-256color","COLORTERM":"truecolor", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1", "CLAUDE_CODE_REMOTE_CONTROL":"0", "ANTHROPIC_BASE_URL":loopback,"ANTHROPIC_API_KEY":"local-only-dummy", "HTTP_PROXY":loopback,"HTTPS_PROXY":loopback,"NO_PROXY":"127.0.0.1,localhost"})
            environment.pop("NO_COLOR", None)
            terminal = Terminal([str(executable), *(["--safe-mode"] if safe_mode else []), "--restricted", "--strict-mcp-config", "--no-chrome", "--permission-mode", "manual", "--setting-sources", "project,local", "--settings", '{"remoteControlAtStartup":false}', "--tools", "", "--session-id", str(uuid.uuid4()), "--name", "isolated-import-reference"], workspace, environment, output)
            terminal.stream = TerminalByteStream(terminal.screen)
            for attempt in range(8):
                terminal.read(1.5)
                visible = terminal.visible()
                if "custom API key" in visible:
                    os.write(terminal.master, b"\x1b[A\r")
                elif "trust this folder" in visible.lower():
                    os.write(terminal.master, b"\x1b[B\r")
                elif any(marker in visible.lower() for marker in ("for shortcuts", "shift+tab", "try ")):
                    break
                elif attempt == 7:
                    raise RuntimeError("isolated startup did not reach a recognizable composer")
            terminal.capture("00-idle")
            os.write(terminal.master, f"/{command}".encode())
            terminal.read(1)
            menu = terminal.capture(f"01-{command}-command-menu")
            # Never press Enter on unrecognized slash text: that could become
            # model input. Only an explicitly displayed local command is opened.
            available = any(text in " ".join(menu.lower().split()) for text in ["import configuration from", "import config from", "import settings from"])
            if command == "cd":
                exact_rows = [line.strip() for line in menu.splitlines()
                              if re.match(r"^\s*(?:[>❯›]\s*)?/cd\s+\S", line)]
                result["status"] = "cd_advertised_in_isolated_reference" if exact_rows else "cd_not_advertised_in_isolated_reference"
                result["exact_command_rows"] = exact_rows
                result["no_commands_match"] = 'No commands match "/cd"' in " ".join(menu.split())
                # An availability query never submits /cd, even if the catalog
                # advertises it. It cannot become a model prompt or change cwd.
                if cd_journey:
                    if not exact_rows or "Move this session to a new working directory" not in menu:
                        raise RuntimeError("Exact local /cd command unavailable; nothing submitted")
                    exercise_cd(terminal, workspace, result)
                else:
                    os.write(terminal.master, b"\x1b\x15")
            elif available:
                os.write(terminal.master, b"\r")
                terminal.read(2)
                opened = terminal.capture("02-import-open")
                result["status"] = "local_import_surface_opened"
                result["reference_surface"] = opened
                os.write(terminal.master, b"\x1b")
            else:
                result["status"] = "import_not_advertised_in_isolated_reference"
                os.write(terminal.master, b"\x1b\x15")
            inference = [entry for entry in requests if entry["method"] == "POST" and "/messages" in entry["path"]]
            assert not inference, inference
            result["local_requests"] = requests
    except Exception as error:
        result.update({"status":"blocked", "error":str(error)})
        if terminal: terminal.capture("blocked")
    finally:
        if terminal: terminal.close()
        server.shutdown()
        server.server_close()
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--command", choices=("import", "cd"), default="import")
    parser.add_argument("--cd-journey", action="store_true", help="Exercise only the observed local cd command against owned child paths")
    parser.add_argument("--without-safe-mode", action="store_true", help="Keep the same isolated empty config, restricted workspace, strict MCP and no-tool bounds, but test the normal command catalog")
    args = parser.parse_args()
    if args.cd_journey and args.command != "cd":
        parser.error("--cd-journey requires --command cd")
    print(json.dumps(run(args.output, not args.without_safe_mode, args.command, args.cd_journey), indent=2))
