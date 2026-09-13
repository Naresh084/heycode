#!/usr/bin/env python3
"""Exercise `/memory` and `/reload-skills` through the actual heycode CLI.

The provider is a deterministic localhost SSE fixture. The production CLI,
composition, command registry, workspace authority, renderer, prompt builder,
skill loader, and session store are real. Every filesystem mutation stays in a
disposable directory; no paid provider or external service is contacted.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import struct
import tempfile
import termios
import threading
import time

import pyte

from terminal_screenshot import TerminalByteStream, render_screen
from tui_blackbox import FullScreenTui


MODEL = {
    "id": "z-ai/glm-5.3-flash",
    "canonical_slug": "z-ai/glm-5.3-flash",
    "name": "Local memory and skill fixture",
    "created": 1787752741,
    "description": "Deterministic localhost-only acceptance provider",
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


class Screen(pyte.Screen):
    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and 1049 in modes:
            self.reset()
        return super().set_mode(*modes, **kwargs)


def write_skill(root: Path, name: str, description: str, body: str) -> None:
    directory = root / name
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n---\n{body}\n"
    )


def revision(source_id: str, root: Path, text: str) -> str:
    stat = root.stat()
    digest = hashlib.sha256()
    digest.update(source_id.encode())
    digest.update(b"\0")
    digest.update(f"{stat.st_dev}:{stat.st_ino}".encode())
    digest.update(b"\0")
    digest.update(text.encode())
    return digest.hexdigest()


def request_with(requests: list[dict[str, object]], marker: str) -> dict[str, object]:
    for request in requests:
        if marker in json.dumps(request, ensure_ascii=False):
            return request
    raise AssertionError(f"no provider request contained {marker!r}")


def system_content(request: dict[str, object]) -> str:
    messages = request.get("messages", [])
    assert isinstance(messages, list), request
    return "\n".join(
        str(message.get("content", ""))
        for message in messages
        if isinstance(message, dict) and message.get("role") == "system"
    )


def run(binary: Path, output: Path, *, visual_only: bool = False, theme: str = "heycode-dark", color: bool = True) -> dict[str, object]:
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
                self.respond({"data": {"label": "local-memory-skill-fixture"}})
            elif "/model/" in self.path:
                self.respond({"data": MODEL})
            else:
                self.respond({"data": [MODEL], "total_count": 1, "links": {"next": None}})

        def do_POST(self):
            length = int(self.headers["Content-Length"])
            request = json.loads(self.rfile.read(length))
            requests.append(request)
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            chunks = [
                {
                    "id": f"memory-skill-{len(requests)}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [
                        {
                            "index": 0,
                            "delta": {"content": f"FIXTURE_REPLY_{len(requests)}"},
                            "finish_reason": None,
                        }
                    ],
                },
                {
                    "id": f"memory-skill-{len(requests)}",
                    "object": "chat.completion.chunk",
                    "model": MODEL["id"],
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                },
            ]
            try:
                for chunk in chunks:
                    self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    captures: list[str] = []
    assertions: list[str] = []
    tui: FullScreenTui | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-memory-skills-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            nested = workspace / "nested"
            home.mkdir()
            workspace.mkdir()
            nested.mkdir()
            (home / "settings.toml").write_text(f'schema_version = 1\n[settings.ui-preferences]\ntheme = "{theme}"\n')
            initial_guidance = "INITIAL_PROJECT_GUIDANCE"
            updated_guidance = "UPDATED_PROJECT_GUIDANCE"
            (workspace / "AGENTS.md").write_text(initial_guidance)
            skills = workspace / ".heycode" / "skills"
            write_skill(skills, "alpha", "Initial alpha catalog", "INITIAL_ALPHA_BODY")
            if visual_only:
                write_skill(skills, "beta", "Second skill catalog", "BETA_BODY")
            auto_memory = home / "sessions" / ".agent-memory" / "user" / "reviewer"
            auto_memory.mkdir(parents=True)
            (auto_memory / "MEMORY.md").write_text("PERSISTENT_REVIEWER_NOTES")

            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                f'model = "{MODEL["id"]}"\n'
                'api_key_env = "HEYCODE_MEMORY_SKILL_FIXTURE"\n'
                f'base_url = "{base}"\n'
            )
            os.environ["HEYCODE_MEMORY_SKILL_FIXTURE"] = "local-only-not-a-real-key"
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
                    "llm.api_key_env=HEYCODE_MEMORY_SKILL_FIXTURE",
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

            def wait_idle(timeout: float = 45) -> str:
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    read()
                    text = visible()
                    if "Responding…" not in text:
                        return text
                    if not tui.alive():
                        raise AssertionError(
                            f"CLI exited while waiting for idle: {text[-1800:]}"
                        )
                raise AssertionError(
                    f"timed out waiting for the completed response: {visible()[-1800:]}"
                )

            def send(command: str, needle: str) -> str:
                os.write(tui.fd, command.encode() + b"\r")
                return wait(needle)

            def capture(name: str) -> None:
                read(0.5)
                text = visible()
                (output / f"{name}.txt").write_text(text)
                render_screen(screen, output / f"{name}.png", background="#f8f9fb" if theme == "heycode-light" else "#101014", foreground="#20242c" if theme == "heycode-light" else "#dddddd")
                captures.append(name)

            def resize(columns: int, rows: int) -> None:
                screen.resize(lines=rows, columns=columns)
                fcntl.ioctl(
                    tui.fd,
                    termios.TIOCSWINSZ,
                    struct.pack("HHHH", rows, columns, 0, 0),
                )
                read(0.7)

            wait("? for shortcuts")
            capture("00-start")

            if visual_only:
                send("/memory", "Project instructions")
                capture("01-memory-chooser")
                resize(60, 24)
                capture("02-memory-narrow")
                resize(126, 46)
                os.write(tui.fd, b"\r")
                wait(initial_guidance)
                capture("03-memory-selected-source")
                send("/skills", "Skills")
                capture("04-skills-on")
                assert "✔ on" in visible() and "alpha" in visible() and "beta" in visible()
                resize(60, 24)
                assert "enter/space to cycle" in visible() and "Esc to close" in visible()
                capture("04a-skills-narrow")
                resize(126, 46)
                target_row = next(i for i,line in enumerate(screen.display) if "beta" in line and "tok" in line)
                os.write(tui.fd, f"\x1b[<0;28;{target_row+1}M\x1b[<0;28;{target_row+1}m".encode())
                read(.3)
                selected = [line for line in screen.display if "❯" in line and "tok" in line]
                assert len(selected) == 1 and "alpha" in selected[0], selected
                capture("04b-skills-pointer")
                os.write(tui.fd, b"\x1b[200~UNFOCUSED_SKILL_PASTE\x1b[201~")
                wait("UNFOCUSED_SKILL_PASTE")
                assert "type to filter" in visible()
                capture("04c-skills-paste-search")
                os.write(tui.fd, b"\x1b")
                read(.3)
                assert "UNFOCUSED_SKILL_PASTE" not in visible() and "type to filter" in visible()
                capture("04d-skills-search-cleared")
                os.write(tui.fd, b"\r")
                wait("enter/space to cycle")
                assert "✔ on" in visible()
                for ordinal, admission in enumerate(("name-only", "user-only", "off"), 5):
                    os.write(tui.fd, b"\r")
                    marker = {"name-only":"●", "user-only":"◯", "off":"✘"}[admission]
                    wait(f"{marker} {admission}")
                    capture(f"{ordinal:02d}-skills-{admission}")
                os.write(tui.fd, b"/beta")
                wait("1/2 skills")
                capture("07a-skills-search")
                os.write(tui.fd, b"\x1b")
                read(.2)
                os.write(tui.fd, b"\x1b")
                read(.2)
                os.write(tui.fd, b"\x1b")
                read(.3)
                send("/skills", "Skills")
                os.write(tui.fd, b"t")
                wait("sorted by tokens")
                capture("07b-skills-sort")
                os.write(tui.fd, b"\x1b[C")
                read(.3)
                capture("08-skills-details")
                resize(60, 24)
                capture("09-skills-narrow")
                resize(126, 46)
                os.write(tui.fd, b"\x1b")
                read(.3)
                send("/skill-doctor", "Skills loaded this session")
                capture("10-skill-stats")
                assert not requests, requests
                result = {"status":"passed", "scope":"visual-only command correction", "theme":theme, "color":color, "binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(), "provider_requests":0, "captures":captures, "model_prompt_sent":False}
                (output / "result.json").write_text(json.dumps(result,indent=2))
                return result

            os.write(tui.fd, b"/memory")
            read(0.7)
            capture("00b-memory-command-menu")
            os.write(tui.fd, b"\r")
            chooser = wait("Project instructions")
            for expected in (
                "Project instructions",
                "User instructions",
                "Saved in ./AGENTS.md",
                "Not created",
                "Auto-memory: 1 source",
            ):
                assert expected in chooser, (expected, chooser)
            for hidden_id in ("project:agents", "user:agents", "auto:user:reviewer"):
                assert hidden_id not in chooser, (hidden_id, chooser)
            assert "revision:" not in chooser
            assert len(requests) == 0
            assertions.append(
                "bare memory opened the compact attributed chooser without inference"
            )
            capture("01-memory-chooser")

            resize(60, 24)
            narrow = visible()
            assert "Memory" in narrow and "Project instructions" in narrow
            assert "Enter show" in narrow
            capture("01b-memory-chooser-narrow")
            resize(126, 46)
            assertions.append("memory chooser retained its selected row and controls at 60x24")

            os.write(tui.fd, b"\r")
            shown = wait(initial_guidance)
            assert "source-id: project:agents" in shown
            assert "revision:" in shown
            assert len(requests) == 0
            assertions.append(
                "keyboard selection opened the exact source through the shared show renderer"
            )
            capture("02-memory-show")

            listing = send("/memory list", "Memory sources:")
            for expected in (
                "project:agents",
                "project / instructions",
                "auto:user:reviewer",
                "user / auto-memory",
            ):
                assert expected in listing, (expected, listing)
            list_rows = listing.rsplit("Memory sources:", 1)[-1].split(
                "Standing instructions", 1
            )[0]
            assert "revision" not in list_rows
            assert len(requests) == 0
            assertions.append(
                "explicit memory list remained accessible and kept revisions in show"
            )
            capture("02b-memory-list")

            os.write(tui.fd, b"/memory\r")
            time.sleep(0.2)
            pointer_panel = wait("click select")
            assert "Project instructions" in pointer_panel
            # Click the second row at its one-based xterm coordinates. A click
            # selects only; it never reveals the source or mutates the draft.
            target_row = next(i for i,line in enumerate(screen.display) if "2. User instructions" in line)
            os.write(tui.fd, f"\x1b[<0;5;{target_row+1}M\x1b[<0;5;{target_row+1}m".encode())
            read(0.5)
            clicked = visible()
            assert "› 2. User instructions" in clicked, clicked
            assert "click select" in clicked
            os.write(tui.fd, b"\x1b[200~/quit\n\x1b[201~")
            read(0.3)
            assert "click select" in visible()
            os.write(tui.fd, b"\x1b")
            read(0.7)
            cancelled = visible()
            assert "click select" not in cancelled
            assert "/quit" not in cancelled
            assert len(requests) == 0
            assertions.append(
                "actual mouse selection, modal paste consumption, and Esc cancellation changed nothing"
            )
            capture("02c-memory-pointer-cancelled")

            initial_revision = revision("project:agents", workspace.resolve(), initial_guidance)
            send(
                f"/memory replace project:agents {initial_revision} {updated_guidance}",
                "next request renders the new source",
            )
            assert (workspace / "AGENTS.md").read_text() == updated_guidance
            assert len(requests) == 0
            assertions.append("revision-checked instruction replacement persisted without inference")
            capture("03-memory-replaced")

            send("MEMORY_NEXT_REQUEST", "FIXTURE_REPLY_1")
            wait_idle()
            memory_request = request_with(requests, "MEMORY_NEXT_REQUEST")
            memory_wire = json.dumps(memory_request, ensure_ascii=False)
            assert updated_guidance in memory_wire
            assert initial_guidance not in memory_wire
            assertions.append("next provider request used the replaced standing instruction")
            capture("04-memory-next-request")

            write_skill(
                skills,
                "alpha",
                "Updated alpha catalog",
                "UPDATED_ALPHA_BODY",
            )
            write_skill(skills, "beta", "Added beta catalog", "BETA_BODY")
            os.write(tui.fd, b"/reload-skills")
            read(0.7)
            capture("04b-reload-skills-command-menu")
            os.write(tui.fd, b"\r")
            reloaded = wait("skills generation")
            assert "1 added" in reloaded and "1 updated" in reloaded
            assert "model catalog changed" in reloaded
            assert len(requests) == 1
            assertions.append("skill rescan reported exact successful delta without inference")
            capture("05-skills-reloaded")

            skills_panel = send("/skills", "2 skills")
            assert "✔ on" in skills_panel
            assert "alpha" in skills_panel and "beta" in skills_panel
            assert len(requests) == 1
            capture("05b-skills-panel-enabled")

            resize(60, 24)
            narrow_skills = visible()
            assert "Skills" in narrow_skills and "alpha" in narrow_skills
            assert "enter/space to cycle" in narrow_skills
            capture("05ba-skills-panel-narrow")
            resize(126, 46)
            assertions.append(
                "skills panel retained its selected row and controls at 60x24"
            )

            os.write(tui.fd, b"/")
            read(0.2)
            os.write(tui.fd, b"\x1b[200~beta\x1b[201~")
            searched = wait("1/2 skills")
            assert "beta" in searched
            assert len(requests) == 1
            assertions.append("skills search consumed bracketed paste without inference")
            capture("05c-skills-search")
            for _ in range(3):
                os.write(tui.fd, b"\x1b")
                read(0.2)
            read(0.5)

            skills_panel = send("/skills", "2 skills")
            os.write(tui.fd, b"t")
            sorted_panel = wait("sorted by tokens")
            assert "sorted by tokens" in sorted_panel
            assert len(requests) == 1
            assertions.append("skills sort changed through the captured t binding")
            capture("05d-skills-sort-tokens")
            os.write(tui.fd, b"\x1b")
            read(0.8)

            skills_panel = send("/skills", "sorted by tokens")
            os.write(tui.fd, b"\r")
            name_only_panel = wait("● name-only")
            assert "● name-only" in name_only_panel
            settings_path = home / "settings.toml"
            settings_text = settings_path.read_text()
            assert 'name_only = ["alpha"]' in settings_text
            assert 'user_only = ["alpha"]' not in settings_text
            assert 'disabled = ["alpha"]' not in settings_text
            capture("05e-skills-alpha-name-only")
            os.write(tui.fd, b"\x1b")
            read(0.5)

            send("NAME_ONLY_CATALOG_REQUEST", "FIXTURE_REPLY_2")
            wait_idle()
            name_only_request = request_with(requests, "NAME_ONLY_CATALOG_REQUEST")
            name_only_system = system_content(name_only_request)
            assert "\n- alpha\n" in name_only_system
            assert "Updated alpha catalog" not in name_only_system
            assert "Added beta catalog" in name_only_system
            assertions.append(
                "name-only kept the invocation name but removed its description from the provider catalog"
            )
            capture("05e1-skills-alpha-name-only-next-request")

            send("/skill alpha NAME_ONLY_SKILL_REQUEST", "FIXTURE_REPLY_3")
            wait_idle()
            name_only_skill_request = request_with(requests, "NAME_ONLY_SKILL_REQUEST")
            assert "UPDATED_ALPHA_BODY" in json.dumps(
                name_only_skill_request, ensure_ascii=False
            )
            assertions.append(
                "name-only still admitted explicit skill loading on the real provider path"
            )
            capture("05e2-skills-alpha-name-only-invoked")

            skills_panel = send("/skills", "sorted by tokens")
            assert "● name-only" in skills_panel
            os.write(tui.fd, b"\r")
            user_only_panel = wait("◯ user-only")
            assert "◯ user-only" in user_only_panel
            settings_text = settings_path.read_text()
            assert 'name_only = ["alpha"]' not in settings_text
            assert 'user_only = ["alpha"]' in settings_text
            assert 'disabled = ["alpha"]' not in settings_text
            capture("05ea-skills-alpha-user-only")
            os.write(tui.fd, b"\x1b")
            read(0.5)

            send("USER_ONLY_CATALOG_REQUEST", "FIXTURE_REPLY_4")
            wait_idle()
            user_only_request = request_with(requests, "USER_ONLY_CATALOG_REQUEST")
            user_only_system = system_content(user_only_request)
            assert "\n- alpha" not in user_only_system
            assert "Updated alpha catalog" not in user_only_system
            assert "Added beta catalog" in user_only_system
            assertions.append(
                "user-only removed the skill entirely from the provider-visible catalog"
            )
            capture("05ea1-skills-alpha-user-only-next-request")

            send("/skill alpha USER_ONLY_SKILL_REQUEST", "FIXTURE_REPLY_5")
            wait_idle()
            user_only_skill_request = request_with(requests, "USER_ONLY_SKILL_REQUEST")
            assert "UPDATED_ALPHA_BODY" in json.dumps(
                user_only_skill_request, ensure_ascii=False
            )
            assertions.append(
                "user-only retained explicit human /skill invocation on the real provider path"
            )
            capture("05ea2-skills-alpha-user-only-invoked")

            skills_panel = send("/skills", "sorted by tokens")
            assert "◯ user-only" in skills_panel
            os.write(tui.fd, b"\r")
            disabled_panel = wait("✘ off")
            assert "✘ off" in disabled_panel
            assert len(requests) == 5
            settings_text = settings_path.read_text()
            assert "[settings.skills-preferences]" in settings_text
            assert 'name_only = ["alpha"]' not in settings_text
            assert 'user_only = ["alpha"]' not in settings_text
            assert 'disabled = ["alpha"]' in settings_text
            assert 'sort = "tokens"' in settings_text
            (output / "settings-after-disable.toml").write_text(settings_text)
            assertions.append(
                "skills Enter cycle durably wrote name-only, user-only, off, and token sort"
            )
            capture("05eb-skills-alpha-off")
            os.write(tui.fd, b"\x1b")
            read(0.8)

            doctor = send("/skill-doctor", "Skills loaded this session")
            assert "alpha [off]" in doctor
            assert "beta [on]" in doctor
            assert "7d tokens" in doctor and "unavailable" in doctor
            assert "uses = this session" in doctor
            assert len(requests) == 5
            assertions.append(
                "skill doctor opened native session-local Stats with unavailable history explicit"
            )
            capture("05ea-skill-doctor-disabled")
            os.write(tui.fd, b"\x1b")
            read(0.5)

            # Reopen only after the provider has observed its own atomic write,
            # then replace the same disposable settings file externally. The
            # still-open panel must refuse to apply its now-stale snapshot.
            stale_panel = send("/skills", "2 skills")
            assert "sorted by tokens" in stale_panel
            settings_text = settings_path.read_text()
            assert 'sort = "tokens"' in settings_text
            next_settings = settings_text.replace('sort = "tokens"', 'sort = "name"')
            assert next_settings != settings_text
            staged_settings = settings_path.with_suffix(".next")
            staged_settings.write_text(next_settings)
            os.replace(staged_settings, settings_path)
            read(1.0)
            os.write(tui.fd, b"\r")
            stale_refusal = wait("changed since")
            assert "alpha" in stale_refusal and "off" in stale_refusal
            assert len(requests) == 5
            assertions.append(
                "an externally replaced settings generation made the open panel refuse its stale toggle"
            )
            capture("05f-skills-stale-toggle-refused")
            os.write(tui.fd, b"\x1b")
            read(0.5)

            send("SKILL_CATALOG_NEXT_REQUEST", "FIXTURE_REPLY_6")
            wait_idle()
            catalog_request = request_with(requests, "SKILL_CATALOG_NEXT_REQUEST")
            catalog_wire = json.dumps(catalog_request, ensure_ascii=False)
            assert "Added beta catalog" in catalog_wire
            assert "Updated alpha catalog" not in catalog_wire
            assert "Initial alpha catalog" not in catalog_wire
            assertions.append(
                "next provider request excluded the disabled skill from the model-visible catalog"
            )
            capture("06-skills-next-request")

            refused = send("/skill alpha DISABLED_SKILL_REQUEST", "disabled")
            assert "open /skills to enable it" in refused
            assert len(requests) == 6
            assertions.append("explicit /skill invocation also refused the disabled skill")
            capture("06b-disabled-skill-invocation-refused")

            enabled_panel = send("/skills", "skills · enter/space")
            assert "✘ off" in enabled_panel
            os.write(tui.fd, b"\r")
            enabled_panel = wait("✔ on")
            assert "✔ on" in enabled_panel
            assert len(requests) == 6
            assertions.append("the persisted toggle could re-enable the same canonical skill")
            capture("06c-skills-alpha-reenabled")
            os.write(tui.fd, b"\x1b")
            read(0.8)

            saved_skills = workspace / ".heycode" / "skills.saved"
            skills.rename(saved_skills)
            outside_skills = root / "outside-skills"
            write_skill(
                outside_skills,
                "alpha",
                "Untrusted replacement",
                "UNTRUSTED_ALPHA_BODY",
            )
            skills.symlink_to(outside_skills, target_is_directory=True)
            failed = send("/reload-skills", "existing skills were retained")
            assert "skill rescan failed" in failed
            assert len(requests) == 6
            assertions.append("unsafe failed rescan retained the complete prior generation")
            capture("07-skills-failed-reload")

            send("/skill alpha FAILED_RELOAD_PRESERVATION", "FIXTURE_REPLY_7")
            wait_idle()
            retained_request = request_with(requests, "FAILED_RELOAD_PRESERVATION")
            retained_wire = json.dumps(retained_request, ensure_ascii=False)
            assert "UPDATED_ALPHA_BODY" in retained_wire
            assert "UNTRUSTED_ALPHA_BODY" not in retained_wire
            assertions.append("model-scheduling /skill still used the retained safe body")
            capture("08-skills-retained-body")

            skills.unlink()
            saved_skills.rename(skills)
            retired_skills = root / "retired-skills"
            skills.rename(retired_skills)
            cleared = send("/reload-skills", "2 removed")
            assert "model catalog changed" in cleared
            assert len(requests) == 7
            assertions.append(
                "project skill generation was explicitly cleared before changing workspace"
            )
            capture("09-skills-cleared")

            send("/cd nested", "Workspace:")
            unavailable = send("/reload-skills", "workspace changed")
            assert "recompose before reloading skills" in unavailable
            assert len(requests) == 7
            assertions.append("post-cd reload was visibly unavailable and scheduled no inference")
            capture("10-reload-after-cd")

            events: list[dict[str, object]] = []
            for path in sorted((home / "sessions").rglob("session.jsonl")):
                events.extend(
                    json.loads(line) for line in path.read_text().splitlines() if line.strip()
                )
            (output / "events.json").write_text(json.dumps(events, indent=2))
            result = {
                "status": "passed",
                "provider": "localhost deterministic SSE fixture",
                "provider_requests": len(requests),
                "captures": captures,
                "assertions": assertions,
                "live_claude_ui_compared": False,
            }
            (output / "result.json").write_text(json.dumps(result, indent=2))
            return result
    except Exception as error:
        if tui is not None and "screen" in locals():
            (output / "failure.txt").write_text("\n".join(screen.display))
            render_screen(screen, output / "failure.png")
        result = {"status":"failed", "binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(), "provider_requests":len(requests), "captures":captures, "assertions":assertions, "failure":f"{type(error).__name__}: {error}"}
        (output / "result.json").write_text(json.dumps(result,indent=2))
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
    parser.add_argument("--visual-only", action="store_true")
    parser.add_argument("--theme", choices=("heycode-dark", "heycode-light"), default="heycode-dark")
    parser.add_argument("--no-color", action="store_true")
    arguments = parser.parse_args()
    outcome = run(arguments.binary, arguments.output, visual_only=arguments.visual_only, theme=arguments.theme, color=not arguments.no_color)
    print(json.dumps(outcome, indent=2))
