#!/usr/bin/env python3
"""Exercise installed-plugin and plugin-contributed-skill UI states.

Two declarative packages are installed into a disposable HEYCODE_HOME through
the real CLI before the TUI starts. The interactive process uses only a local
catalog fixture and must make zero inference requests.
"""

from __future__ import annotations

import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import platform
import struct
import subprocess
import tempfile
import termios
import threading
import time

from memory_skills_pty import MODEL, Screen
from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


def platform_words() -> tuple[str, str]:
    os_name = {
        "Darwin": "macos",
        "Linux": "linux",
        "FreeBSD": "freebsd",
    }.get(platform.system())
    architecture = {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "x86_64": "x86_64",
        "AMD64": "x86_64",
    }.get(platform.machine())
    if os_name is None or architecture is None:
        raise RuntimeError(f"unsupported fixture platform: {platform.platform()}")
    return os_name, architecture


def write_package(root: Path, name: str, permissions: list[str]) -> Path:
    source = root / f"package-{name}"
    (source / ".heycode-plugin").mkdir(parents=True)
    (source / "skills").mkdir()
    os_name, architecture = platform_words()
    requested = ", ".join(json.dumps(permission) for permission in permissions)
    credentials = (
        '[{ reference = "acme/visibility/api", kind = "api-key" }]'
        if name == "visibility"
        else "[]"
    )
    policy = "required" if name == "visibility" else "none"
    (source / ".heycode-plugin" / "plugin.toml").write_text(
        f'''schema_version = 1
id = "acme/{name}"
name = "{name.title()} fixture"
version = "1.0.0"
description = "Installed capability visibility fixture."
license = "MIT"
default_enabled = true
requested_permissions = [{requested}]
platforms = [{{ os = "{os_name}", architecture = "{architecture}" }}]
dependencies = []
conflicts = []

[[contributions]]
kind = "skill"
id = "review"
path = "skills/review.md"
exposure = {{ mode = "namespaced" }}

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "fixture/{name}"
revision = "test-v1"
update_channel = "pinned"

[authentication]
policy = "{policy}"
credentials = {credentials}
'''
    )
    (source / "skills" / "review.md").write_text(
        "---\n"
        f"name: {name}-review\n"
        f"description: Review through installed {name}\n"
        "---\n"
        f"INSTALLED_{name.upper()}_SKILL_BODY\n"
    )
    return source


def run(binary: Path, output: Path, *, visual_only: bool = False) -> dict[str, object]:
    output.mkdir(parents=True, exist_ok=True)
    posts: list[dict[str, object]] = []

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
                self.respond({"data": {"label": "local-plugin-ui-fixture"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            posts.append(json.loads(self.rfile.read(length)))
            self.send_error(500, "this acceptance journey forbids inference")

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    captures: list[str] = []
    assertions: list[str] = []
    gaps: list[str] = []
    tui: FullScreenTui | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-plugin-skills-ui-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_PLUGIN_UI_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            environment = dict(os.environ)
            environment["HEYCODE_HOME"] = str(home)
            environment["HEYCODE_PLUGIN_UI_FIXTURE"] = "local-only-not-a-real-key"
            installs = []
            for name, permissions in [
                ("alpha", []),
                ("visibility", ["filesystem_read", "network_access", "credential_use"]),
            ]:
                package = write_package(root, name, permissions)
                completed = subprocess.run(
                    [
                        str(binary.resolve()),
                        "--restricted-workspace",
                        "plugin",
                        "install",
                        str(package),
                    ],
                    cwd=workspace,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                installs.append(
                    {
                        "id": f"acme/{name}",
                        "exit": completed.returncode,
                        "stdout": completed.stdout,
                        "stderr": completed.stderr,
                    }
                )
                if completed.returncode != 0:
                    raise AssertionError(installs[-1])
            (output / "install-results.json").write_text(json.dumps(installs, indent=2))

            os.environ["HEYCODE_PLUGIN_UI_FIXTURE"] = "local-only-not-a-real-key"
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
                    "llm.api_key_env=HEYCODE_PLUGIN_UI_FIXTURE",
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

            def capture(name: str) -> str:
                read(0.5)
                text = visible()
                (output / f"{name}.txt").write_text(text)
                render_screen(screen, output / f"{name}.png")
                captures.append(name)
                return text

            def resize(columns: int, rows: int) -> None:
                screen.resize(lines=rows, columns=columns)
                fcntl.ioctl(
                    tui.fd,
                    termios.TIOCSWINSZ,
                    struct.pack("HHHH", rows, columns, 0, 0),
                )
                read(0.7)

            wait("shift+tab to cycle")
            capture("00-start")

            if visual_only:
                os.write(tui.fd, b"/plugin\r")
                wait("Installed packages")
                initial = capture("01-installed-list")
                assert "acme/alpha" in initial and "Search" in initial
                os.write(tui.fd, b"\r")
                wait("Version:")
                capture("02-plugin-details")
                os.write(tui.fd, b"i")
                read(.3)
                capture("03-plugin-provenance")
                os.write(tui.fd, b"\t")
                read(.3)
                capture("04-plugin-permissions")
                os.write(tui.fd, b"\x1b")
                wait("Installed packages")
                os.write(tui.fd, b"visibility")
                read(.3)
                filtered = capture("05-plugin-search")
                assert "acme/visibility" in filtered and "acme/alpha" not in filtered
                resize(60, 24)
                capture("06-plugin-list-narrow")
                os.write(tui.fd, b"\r")
                wait("Version:")
                capture("07-plugin-details-narrow")
                assert not posts, posts
                result = {"status":"passed", "scope":"visual-only Installed list and details correction", "binary_sha256":__import__("hashlib").sha256(binary.read_bytes()).hexdigest(), "provider_requests":0, "captures":captures, "model_prompt_sent":False}
                (output / "result.json").write_text(json.dumps(result,indent=2))
                return result
            os.write(tui.fd, b"/plugin")
            read(0.7)
            capture("01-plugin-alias-command-menu")
            os.write(tui.fd, b"\r")
            provenance = wait("plugins (2)")
            for expected in (
                "acme/alpha",
                "acme/visibility",
                "contributions declared: 1",
                "origin: local directory fixture/alpha",
                "update channel: pinned",
                "revision: test-v1",
            ):
                assert expected in provenance, (expected, provenance)
            assert not posts
            assertions.append(
                "plugin alias opened attributed installed-package provenance without inference"
            )
            capture("02-plugin-provenance")

            os.write(tui.fd, b"\x1b[6~")
            provenance_tail = wait("signature: none attached")
            assert "checksum: none declared" in provenance_tail
            capture("02b-plugin-provenance-details-tail")
            os.write(tui.fd, b"\x1b[5~")
            read(0.4)

            resize(60, 30)
            narrow = capture("03-plugin-provenance-narrow")
            assert "Installed plugins" in narrow and "acme/alpha" in narrow
            assertions.append("installed-plugin panel remained usable at 60x30")
            resize(126, 46)

            # SGR mouse wheel down: button 65, one-based column/row.
            os.write(tui.fd, b"\x1b[<65;10;5M")
            selected = wait("origin: local directory fixture/visibility")
            assert "acme/visibility" in selected
            assertions.append("mouse wheel moved the plugin selection without touching the transcript")
            capture("04-plugin-pointer-selection")

            os.write(tui.fd, b"\t")
            permissions = wait("requests network_access")
            for expected in (
                "requests filesystem_read",
                "requests network_access",
                "requests credential_use",
                "credentials required",
                "credential reference: acme/visibility/api",
            ):
                assert expected in permissions, (expected, permissions)
            capture("05-plugin-permissions")

            os.write(tui.fd, b"\t")
            enable = wait("manifest asks to be enabled by default")
            assert "state: enabled" in enable
            capture("06-plugin-enable")

            os.write(tui.fd, b"\t")
            update = wait("rollback target: none")
            assert "cache holds: 1.0.0" in update
            capture("07-plugin-update")
            assertions.append(
                "all four installed-package capability sections showed source-backed facts"
            )

            os.write(tui.fd, b"\x1b[C" + b"\x1b[B" * 4 + b"\r")
            refusal = wait("Rollback unavailable")
            assert "has no previous version" in refusal
            capture("08-plugin-action-error")
            assertions.append("unavailable rollback stayed local and rendered a concrete reason")

            os.write(tui.fd, b"\x1b[A\r")
            wait("update `acme/visibility`")
            os.write(tui.fd, b"bad version\r")
            invalid = wait("invalid version")
            assert "esc cancel" in invalid
            capture("09-plugin-form-error")
            os.write(tui.fd, b"\x1b")
            after_cancel = capture("10-plugin-form-cancelled")
            assert "● version" not in after_cancel
            os.write(tui.fd, b"\x1b")
            wait("shift+tab to cycle")
            assert not posts
            assertions.append("invalid form and Esc cancellation changed no installed state")

            os.write(tui.fd, b"/plugins verbose\r")
            verbose = wait("plugin panel-commands@0.1.0")
            assert "command: mcp" in verbose and "command: agents" in verbose
            capture("11-plugins-verbose")
            assertions.append(
                "verbose plugins remained the exact live composition inventory, distinct from installed-package lifecycle state"
            )

            os.write(tui.fd, b"/skills\r")
            skills = wait("Review through installed visibility")
            for expected in (
                "Review through installed alpha",
                "Review through installed visibility",
                "contribution · live registry",
            ):
                assert expected in skills, (expected, skills)
            capture("12-installed-plugin-skills")
            namespaced = ("acme/alpha::review", "acme/visibility::review")
            if not all(expected in skills for expected in namespaced):
                assert skills.count("ext-review-") >= 2, skills
                gaps.append(
                    "installed skill rows use opaque ext-review hashes instead of the manifest namespaced invocation identities"
                )
            os.write(tui.fd, b"\x1b")
            assert not posts
            assertions.append(
                "enabled installed-plugin skills were visible with their descriptions and live-registry source"
            )

            result = {
                "status": "passed_with_gaps" if gaps else "passed",
                "binary": str(binary.resolve()),
                "binary_sha256": __import__("hashlib").sha256(binary.read_bytes()).hexdigest(),
                "provider": "localhost catalog only; inference forbidden",
                "provider_requests": len(posts),
                "installed_plugins": ["acme/alpha", "acme/visibility"],
                "captures": captures,
                "assertions": assertions,
                "gaps": gaps,
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            return result
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
    parser.add_argument("--visual-only", action="store_true")
    arguments = parser.parse_args()
    print(json.dumps(run(arguments.binary, arguments.output, visual_only=arguments.visual_only), indent=2))
