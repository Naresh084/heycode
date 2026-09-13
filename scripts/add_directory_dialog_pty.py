#!/usr/bin/env python3
"""Native no-argument /add-dir PTY acceptance against a pinned disposable CLI.

Uses a temporary home/workspace and a deterministic localhost provider. Existing
workspace evidence remains immutable; this script records the new native dialog.
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

from workspace_transitions_pty import MODEL, Terminal, environment


def run(binary: Path, expected_sha: str, output: Path):
    actual_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    assert actual_sha == expected_sha, f"Unexpected CLI artifact: {actual_sha}"
    output.mkdir(parents=True, exist_ok=False)
    requests: list[dict] = []
    fixture_paths: dict[str, str] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path.endswith("/key"):
                payload = {"data": {"label": "directory-pty-local"}}
            elif "/model/" in self.path:
                payload = {"data": MODEL}
            else:
                payload = {"data": [MODEL], "total_count": 1, "links": {"next": None}}
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            (output / "requests.json").write_text(json.dumps(requests, indent=2))
            messages = request.get("messages", [])
            user = str(next((m.get("content", "") for m in reversed(messages) if m.get("role") == "user"), ""))
            marker = next((m for m in ("PROBE_DENIED", "PROBE_ALLOWED", "PROBE_OUTSIDE", "PROBE_RELOADED") if m in user), "AUXILIARY")
            after_tool = bool(messages and messages[-1].get("role") == "tool")
            if marker in ("PROBE_DENIED", "PROBE_ALLOWED") and not after_tool:
                delta = {"reasoning": "Exercise the actual session file boundary using the local fixture.", "tool_calls": [{"index": 0, "id": f"call-{marker}", "type": "function", "function": {"name": "read", "arguments": json.dumps({"path": fixture_paths["outside_file"]})}}]}
                reason = "tool_calls"
            else:
                content = marker + "_DONE"
                if after_tool:
                    content += " AUTHORIZED_OUTSIDE_FILE" if "AUTHORIZED_OUTSIDE_FILE" in str(messages[-1].get("content", "")) else " READ_REFUSED"
                delta, reason = {"content": content}, "stop"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for piece, finish in ((delta, None), ({}, reason)):
                packet = {"id": f"dialog-{len(requests)}", "model": MODEL["id"], "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": piece, "finish_reason": finish}]}
                self.wfile.write(("data: " + json.dumps(packet) + "\n\n").encode())
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    terminal = None
    result = {"binary_sha256": actual_sha, "commercial_requests": 0, "engine": "heycode", "scope": "native-session-directory-dialog"}
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-add-directory-dialog-") as folder:
            root = Path(folder).resolve()
            home, work, extra, explicit = (root / name for name in ("home", "work", "extra dir", "explicit"))
            for path in (home, work, extra, explicit):
                path.mkdir()
            (work / "AGENTS.md").write_text("ORIGINAL_PROJECT_GUIDANCE")
            (extra / "AGENTS.md").write_text("UNTRUSTED_OUTSIDE_GUIDANCE")
            (extra / "allowed.txt").write_text("AUTHORIZED_OUTSIDE_FILE")
            fixture_paths["outside_file"] = str(extra / "allowed.txt")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(f'''schema_version = 31
[llm]
provider = "openrouter"
model = "{MODEL['id']}"
api_key_env = "HEYCODE_DIRECTORY_PTY_KEY"
base_url = "{base}"
[approval]
mode = "full_access"
''')
            config_before = (home / "config.toml").read_bytes()
            env = environment(home)
            env.update({"HEYCODE_HOME": str(home), "HEYCODE_DIRECTORY_PTY_KEY": "localhost-only-fixture"})
            terminal = Terminal([str(binary.resolve()), "--no-background", "--trust-workspace", "--provider", "openrouter", "--model", MODEL["id"], "--set", f"llm.base_url={base}", "--set", "llm.api_key_env=HEYCODE_DIRECTORY_PTY_KEY"], work, env, output)
            terminal.wait("shift+tab to cycle")
            terminal.send("/rename Directory dialog fixture")
            terminal.wait("Renamed to Directory dialog fixture")
            sessions = list((home / "sessions").rglob("session.jsonl"))
            assert len(sessions) == 1, sessions
            session = sessions[0]
            journal = session.parent / "workspace.json"

            def state():
                return json.loads(journal.read_text()) if journal.exists() else None

            def edit(text: str):
                os.write(terminal.master, b"\x15")  # Ctrl+U: clear prefix.
                os.write(terminal.master, b"\x1b[200~" + text.encode() + b"\x1b[201~")
                terminal.read(0.3)

            def enter():
                os.write(terminal.master, b"\r")
                terminal.read(0.3)

            def open_dialog():
                os.write(terminal.master, b"/add-dir")
                terminal.read(0.5)
                enter()
                terminal.wait("Enter an existing directory path")

            def probe(marker: str):
                start = len(requests)
                terminal.send(marker)
                terminal.wait(marker + "_DONE")
                assert len(requests) > start
                return requests[start:]

            terminal.capture("00-start")
            open_dialog()
            terminal.capture("01-native-path-dialog")
            os.write(terminal.master, b"\x1b")
            terminal.read(0.5)
            assert "Enter an existing directory path" not in terminal.visible()
            assert state() is None
            terminal.capture("02-path-cancel-no-grant")
            draft = "DRAFT_TO_KEEP_after_directory_cancel"
            os.write(terminal.master, draft.encode())
            cursor = len(draft) - 6
            os.write(terminal.master, b"\x1b[D" * 6)
            terminal.read(0.4)
            terminal.capture("02a-composer-draft-before-palette")
            os.write(terminal.master, b"\x10")  # Ctrl+P temporarily sets aside prose.
            terminal.read(0.4)
            os.write(terminal.master, b"add-dir")
            terminal.read(0.4)
            enter()
            terminal.wait("Enter an existing directory path")
            terminal.capture("02b-directory-dialog-from-draft")
            edit(str(extra))
            enter()
            terminal.wait("Grant for this session")
            os.write(terminal.master, b"\x1b")
            terminal.read(0.6)
            restored = terminal.capture("02c-composer-draft-after-cancel")
            assert draft in restored, "Ctrl+P /add-dir cancellation discarded the original composer draft"
            assert state() is None
            os.write(terminal.master, b"<CURSOR>")
            terminal.read(0.4)
            cursor_capture = terminal.capture("02d-composer-cursor-after-cancel")
            assert draft[:cursor] + "<CURSOR>" + draft[cursor:] in cursor_capture, "Directory cancellation changed the original draft cursor"
            os.write(terminal.master, b"\x05\x15")
            terminal.read(0.3)
            os.write(terminal.master, b"PREFIX_COMPLETION_DRAFT")
            terminal.read(0.3)
            os.write(terminal.master, b"\x10")
            terminal.read(0.3)
            os.write(terminal.master, b"add-d")
            terminal.read(0.3)
            enter()  # Complete the prefix to /add-dir without discarding the saved draft.
            enter()
            terminal.wait("Enter an existing directory path")
            os.write(terminal.master, b"\x1b")
            terminal.read(0.5)
            prefix_capture = terminal.capture("02e-prefix-completion-draft-after-cancel")
            assert "PREFIX_COMPLETION_DRAFT" in prefix_capture, "Prefix completion discarded the original composer draft"
            os.write(terminal.master, b"\x05\x15")
            terminal.read(0.3)
            probe("PROBE_DENIED")
            terminal.wait("READ_REFUSED")
            terminal.capture("03-read-before-grant-refused")
            open_dialog()
            edit("bad\npath")
            terminal.wait("without newlines or control characters")
            terminal.capture("04-multiline-paste-refused")
            edit(str(root / "does-not-exist"))
            enter()
            terminal.wait("Directory is unavailable")
            terminal.capture("05-missing-path-inline-error")
            assert state() is None
            edit(str(extra))
            enter()
            terminal.wait("Grant for this session")
            terminal.capture("06-session-only-confirmation")
            assert not requests[-1]["messages"][-1].get("content", "").endswith(str(extra))
            assert state() is None
            os.write(terminal.master, b"\t\r")
            terminal.read(0.6)
            assert "Grant for this session" not in terminal.visible()
            assert state() is None
            terminal.capture("07-confirmation-cancel-no-grant")
            open_dialog()
            edit(str(extra))
            enter()
            terminal.wait("Grant for this session")
            extra.rename(root / "previous-extra")
            extra.mkdir()
            (extra / "AGENTS.md").write_text("UNTRUSTED_OUTSIDE_GUIDANCE")
            (extra / "allowed.txt").write_text("AUTHORIZED_OUTSIDE_FILE")
            enter()
            terminal.wait("Directory changed since it was selected")
            terminal.capture("08-replaced-directory-refused")
            assert state() is None
            enter()
            terminal.wait("Grant for this session")
            enter()
            terminal.wait("to this session's allowed directories")
            terminal.capture("09-session-grant-receipt")
            granted = state()["snapshot"]
            assert granted["revision"] == 1 and granted["cwd"] == str(work)
            assert str(extra) in [r["path"] for r in granted["roots"]]
            probe("PROBE_ALLOWED")
            terminal.wait("PROBE_ALLOWED_DONE AUTHORIZED_OUTSIDE_FILE")
            terminal.capture("10-authorized-file-read")
            terminal.send(f"/cd {extra}")
            terminal.wait("revision 2")
            outside_request = probe("PROBE_OUTSIDE")[0]
            system = "\n".join(str(m.get("content", "")) for m in outside_request["messages"] if m["role"] == "system")
            assert "UNTRUSTED_OUTSIDE_GUIDANCE" not in system and "ORIGINAL_PROJECT_GUIDANCE" not in system
            terminal.capture("11-granted-directory-without-project-trust")
            before_reload = session.read_bytes()
            offset = len(terminal.transcript)
            terminal.send("/reload-plugins")
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                terminal.read()
                raw = terminal.transcript[offset:]
                if b"\x1b[?1049l" in raw and b"\x1b[?1049h" in raw and "shift+tab to cycle" in terminal.visible():
                    break
            else:
                raise AssertionError("reload did not reenter terminal")
            assert list((home / "sessions").rglob("session.jsonl")) == [session]
            assert session.read_bytes().startswith(before_reload)
            assert state()["snapshot"]["cwd"] == str(extra)
            assert state()["snapshot"]["revision"] == 2
            probe("PROBE_RELOADED")
            terminal.capture("12-reload-keeps-session-grant")
            terminal.send(f"/add-dir {explicit}")
            terminal.wait("revision 3")
            terminal.capture("13-explicit-path-remains-direct")
            assert state()["snapshot"]["cwd"] == str(extra)
            assert str(explicit) in [r["path"] for r in state()["snapshot"]["roots"]]
            assert (home / "config.toml").read_bytes() == config_before
            (output / "workspace-final.json").write_text(json.dumps(state(), indent=2))
            (output / "session.jsonl").write_bytes(session.read_bytes())
            result.update({"status": "passed", "session_id": session.parent.name, "localhost_provider_requests": len(requests), "path_cancel_unchanged": True, "confirmation_cancel_unchanged": True, "composer_draft_preserved": True, "composer_cursor_preserved": True, "prefix_completion_draft_preserved": True, "multiline_paste_rejected": True, "missing_inline_error": True, "replaced_candidate_refused": True, "actual_file_read_after_grant": True, "outside_guidance_not_trusted": True, "same_session_reload_preserves_grant": True, "global_config_unchanged": True, "explicit_path_unchanged": True})
    except Exception as error:
        result.update({"status": "failed", "error": f"{type(error).__name__}: {error}"})
        if terminal:
            terminal.capture("failed")
    finally:
        if terminal:
            result["captures"] = terminal.captures
            terminal.close()
        server.shutdown()
        server.server_close()
        (output / "result.json").write_text(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.binary, args.sha256, args.output)
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result["status"] == "passed" else 1)
