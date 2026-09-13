#!/usr/bin/env python3
"""Exercise default/session effort selection in a real TUI with zero inference."""
from __future__ import annotations

import argparse
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
from urllib.parse import unquote

from advisor_lifecycle_terminal_check import Driver, MODELS, MODEL, model, sha256


def run(binary: Path, output: Path) -> dict:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    gets, posts = [], []

    class Handler(http.server.BaseHTTPRequestHandler):
        def send(self, body, status=200):
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            gets.append(self.path)
            if self.path.endswith("/key"):
                self.send({"data": {"label": "local effort fixture"}})
            elif "/model/" in self.path:
                requested = unquote(self.path).split("/model/", 1)[1].split("?", 1)[0]
                self.send({"data": next((row for row in MODELS if row["id"] == requested), model(requested, "Local effort fixture"))})
            else:
                self.send({"data": MODELS, "total_count": len(MODELS), "links": {"next": None}})

        def do_POST(self):
            posts.append(self.path)
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            self.send({"error": {"message": "inference forbidden"}}, 500)

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    previous = os.environ.get("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE")
    os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = "local-only-dummy-key"
    result = {"status": "failed", "binary": str(binary), "binary_sha256": sha256(binary), "provider_requests": posts, "catalog_requests": gets, "captures": [], "checks": {}}
    driver = None
    try:
        with tempfile.TemporaryDirectory(prefix="heycode-effort-scope-") as folder:
            root = Path(folder)
            home, workspace = root / "home", root / "workspace"
            home.mkdir(); workspace.mkdir()
            settings = home / "settings.toml"
            settings.write_text("schema_version = 1\n")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(f'schema_version = 31\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"\nbase_url = "{base}"\n')

            def start(phase):
                instance = Driver(binary=binary, home=home, workspace=workspace, base_url=base, output=output, phase=phase, columns=110, rows=42, timeout=30, model_id=MODEL["id"])
                instance.wait("shift+tab to cycle")
                return instance

            def capture(name):
                result["captures"].append(f"{driver.phase}-{name}")
                return driver.capture(name)

            driver = start("01")
            driver.command("/effort", "s for this session")
            initial = capture("initial")
            assert "max  default" in initial or "max  current · default" in initial
            os.write(driver.tui.fd, b"\x1b[D\r")
            driver.wait("reasoning effort persisted as high")
            persisted = settings.read_bytes()
            assert b'effort = "high"' in persisted
            driver.command("/effort", "s for this session")
            default_ui = capture("saved-default")
            assert "high  current" in default_ui and "default" in default_ui
            os.write(driver.tui.fd, b"\x1b[Ds")
            driver.wait("reasoning effort set to low for this session")
            assert settings.read_bytes() == persisted
            driver.command("/effort", "s for this session")
            session_ui = capture("session-low")
            assert "low  current" in session_ui and "low  current · default" not in session_ui
            os.write(driver.tui.fd, b"\x1b[C")
            preview = capture("preview-default")
            assert "high  default" in preview
            # Source ignores pointer clicks on the scale. Paste must stay in the modal.
            os.write(driver.tui.fd, b"\x1b[<0;4;35M\x1b[<0;4;35m\x1b[200~never-submit-this\x1b[201~")
            pointer = capture("pointer-and-paste")
            assert "high  default" in pointer and "never-submit-this" not in pointer
            driver.escape()
            driver.command("/effort", "s for this session")
            canceled = capture("cancel-preserved-low")
            assert "low  current" in canceled
            driver.escape()
            assert settings.read_bytes() == persisted
            driver.close(); driver = None

            driver = start("02")
            driver.command("/effort", "s for this session")
            restored = capture("restart-saved-high")
            assert "high  current · default" in restored
            assert settings.read_bytes() == persisted
            driver.escape()
            driver.close(); driver = None
            assert not posts
            result["checks"] = {name: True for name in ["backend_exact_choices", "enter_persists_default", "session_selection_no_write", "current_and_saved_default_distinct", "escape_discards_preview", "pointer_ignored", "paste_consumed", "restart_restores_default", "no_inference"]}
            result["status"] = "passed"
    except Exception as error:
        result["failure"] = f"{type(error).__name__}: {error}"
        if driver:
            driver.capture("failure")
    finally:
        if driver:
            driver.close()
        server.shutdown(); server.server_close(); thread.join(timeout=2)
        if previous is None:
            os.environ.pop("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE", None)
        else:
            os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = previous
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.binary, args.output)
    print(json.dumps(result, indent=2))
    raise SystemExit(result["status"] != "passed")
