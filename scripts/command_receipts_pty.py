#!/usr/bin/env python3
"""Capture heycode command echo rows and the receipts that close them.

Each state is produced by the real CLI: a command that reports a settled
outcome, a skills panel dismissed with and without an admission change, a
cancelled memory chooser, and a slash line that names no command. The provider
is a deterministic loopback SSE fixture that must never be reached — the run
asserts zero provider requests. Every filesystem mutation stays inside a
disposable directory.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import time

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui

from memory_skills_pty import MODEL, Screen, write_skill


def run(
    binary: Path,
    output: Path,
    *,
    theme: str = "heycode-dark",
    color: bool = True,
) -> dict[str, object]:
    output.mkdir(parents=True, exist_ok=True)
    requests: list[dict[str, object]] = []

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
                self.respond({"data": {"label": "local-command-receipt-fixture"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            length = int(self.headers["Content-Length"])
            requests.append(json.loads(self.rfile.read(length)))
            self.send_response(500)
            self.end_headers()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    captures: list[str] = []
    receipts: dict[str, str] = {}
    assertions: list[str] = []
    tui: FullScreenTui | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-command-receipts-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "settings.toml").write_text(
                f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n'
            )
            (workspace / "AGENTS.md").write_text("RECEIPT_PROJECT_GUIDANCE")
            skills = workspace / ".heycode" / "skills"
            write_skill(skills, "alpha", "Initial alpha catalog", "INITIAL_ALPHA_BODY")

            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_COMMAND_RECEIPT_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            os.environ["HEYCODE_COMMAND_RECEIPT_FIXTURE"] = "local-only-not-a-real-key"
            tui = FullScreenTui(
                str(home),
                str(workspace),
                str(binary.resolve()),
                fake=False,
                color=color,
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
                    "llm.api_key_env=HEYCODE_COMMAND_RECEIPT_FIXTURE",
                ],
            )
            screen = Screen(126, 46)
            stream = TerminalByteStream(screen)

            def read(seconds: float = 0.2) -> None:
                stream.feed(tui.read(seconds))

            def visible() -> str:
                return "\n".join(screen.display)

            def wait(needle: str, timeout: float = 45) -> str:
                expected = "".join(needle.split())
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    read()
                    text = visible()
                    if expected in "".join(text.split()):
                        return text
                    if not tui.alive():
                        raise AssertionError(
                            f"CLI exited while waiting for {needle!r}: {text[-1800:]}"
                        )
                raise AssertionError(f"timed out waiting for {needle!r}: {visible()[-1800:]}")

            def capture(name: str) -> None:
                read(0.5)
                (output / f"{name}.txt").write_text(visible())
                render_screen(
                    screen,
                    output / f"{name}.png",
                    background="#f8f9fb" if theme == "heycode-light" else "#101014",
                    foreground="#20242c" if theme == "heycode-light" else "#dddddd",
                )
                captures.append(name)

            def receipt_after(command: str) -> str:
                """The `⎿` row directly under the newest echo of `command`."""
                lines = screen.display
                echo = max(
                    index
                    for index, line in enumerate(lines)
                    if line.rstrip() == f"❯ {command}"
                )
                rows = []
                for line in lines[echo + 1 :]:
                    if not line.strip():
                        break
                    rows.append(line.rstrip())
                assert rows, f"no receipt under {command}: {lines[echo:echo + 4]}"
                assert rows[0].startswith("  ⎿  "), rows
                for row in rows[1:]:
                    assert row.startswith("     "), rows
                return " ".join(
                    [rows[0][5:].strip()] + [row[5:].strip() for row in rows[1:]]
                ).strip()

            def send(command: str, needle: str) -> str:
                os.write(tui.fd, command.encode() + b"\r")
                return wait(needle)

            wait("? for shortcuts")
            capture("00-start")

            # A command that reports a settled outcome.
            write_skill(skills, "beta", "Added beta catalog", "BETA_BODY")
            send("/reload-skills", "skills generation")
            capture("01-command-with-receipt")
            receipts["reload-skills"] = receipt_after("/reload-skills")
            assert "1 added" in receipts["reload-skills"], receipts
            assertions.append(
                "a command that changed state closed its echo with one ⎿ receipt row"
            )

            # A cancelled chooser.
            send("/memory", "Project instructions")
            os.write(tui.fd, b"\x1b")
            wait("⎿  Cancelled memory editing")
            capture("02-memory-cancelled")
            receipts["memory"] = receipt_after("/memory")
            assert receipts["memory"] == "Cancelled memory editing", receipts
            assertions.append("dismissing the memory chooser reported the cancellation")

            # A panel dismissed with no admission change. Sorting and search
            # are deliberately exercised first: neither is an admission change.
            send("/skills", "2 skills")
            os.write(tui.fd, b"t")
            wait("sorted by tokens")
            os.write(tui.fd, b"/")
            read(0.2)
            os.write(tui.fd, b"beta")
            wait("1/2 skills")
            for _ in range(3):
                os.write(tui.fd, b"\x1b")
                read(0.3)
            wait("⎿  No changes")
            capture("03-skills-closed-unchanged")
            receipts["skills-unchanged"] = receipt_after("/skills")
            assert receipts["skills-unchanged"] == "No changes", receipts
            assertions.append(
                "sorting and searching a skills panel still closed it as No changes"
            )

            # A panel dismissed after a durable admission change.
            send("/skills", "enter/space to cycle")
            os.write(tui.fd, b"\r")
            wait("● name-only")
            os.write(tui.fd, b"\x1b")
            wait("⎿  alpha is now name-only")
            capture("04-skills-closed-changed")
            receipts["skills-changed"] = receipt_after("/skills")
            assert receipts["skills-changed"] == "alpha is now name-only", receipts
            assertions.append(
                "a durably applied admission change closed the panel by naming it"
            )

            # A slash line that names no command.
            os.write(tui.fd, b"/not-a-command")
            read(0.7)
            capture("05-unknown-command-menu")
            os.write(tui.fd, b"\r")
            wait("Unknown command")
            capture("06-unknown-command-result")
            unknown = [
                line.rstrip()
                for line in screen.display
                if line.strip().startswith("⏺ Unknown command")
            ]
            assert unknown == ["⏺ Unknown command: /not-a-command"], unknown
            receipts["unknown-command"] = unknown[0]
            assert not any(
                "❯ /not-a-command" == line.rstrip() for line in screen.display
            ), "an unrecognised slash line is never echoed as an accepted command"
            assertions.append(
                "an unknown command answered with a standalone ⏺ row and no echo"
            )

            assert not requests, requests
            result = {
                "status": "passed",
                "provider": "localhost deterministic fixture, never reached",
                "provider_requests": len(requests),
                "theme": theme,
                "color": color,
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "captures": captures,
                "receipts": receipts,
                "assertions": assertions,
                "live_claude_ui_compared": False,
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            return result
    except Exception as error:
        if tui is not None and "screen" in locals():
            (output / "failure.txt").write_text("\n".join(screen.display))
            render_screen(screen, output / "failure.png")
        result = {
            "status": "failed",
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "provider_requests": len(requests),
            "captures": captures,
            "receipts": receipts,
            "assertions": assertions,
            "failure": f"{type(error).__name__}: {error}",
        }
        (output / "result.json").write_text(json.dumps(result, indent=2))
        raise
    finally:
        if tui is not None:
            (output / "terminal.ansi").write_bytes(tui.transcript)
            tui.close()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/heycode"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark"
    )
    parser.add_argument("--no-color", action="store_true")
    arguments = parser.parse_args()
    print(
        json.dumps(
            run(
                arguments.binary,
                arguments.output,
                theme=arguments.theme,
                color=not arguments.no_color,
            ),
            indent=2,
        )
    )
