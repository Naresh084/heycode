#!/usr/bin/env python3
"""Exercise default/session model selection in a real TUI with zero inference."""
from __future__ import annotations

import argparse
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
from urllib.parse import unquote

from advisor_lifecycle_terminal_check import Driver, MODELS as BASE_MODELS, MODEL, MAIN_MODEL, ADVISOR_MODEL, model, sha256

# OpenRouter publishes a reasoning vocabulary per model. These two rows keep the
# journey honest about that: one names a scale the strict GLM route does not
# have, the other reports reasoning without naming any effort at all.
GRADED_MODEL = "local/graded-effort-model"
UNPUBLISHED_EFFORT_MODEL = "local/unpublished-effort-model"


def reasoning_model(model_id: str, name: str, reasoning: dict) -> dict:
    value = model(model_id, name)
    value["reasoning"] = reasoning
    return value


MODELS = [
    *BASE_MODELS,
    reasoning_model(
        GRADED_MODEL,
        "Local graded effort fixture",
        {
            "mandatory": False,
            "default_enabled": True,
            "supported_efforts": ["low", "medium", "high"],
            "default_effort": "medium",
        },
    ),
    reasoning_model(
        UNPUBLISHED_EFFORT_MODEL,
        "Local unpublished effort fixture",
        {"mandatory": False, "default_enabled": True},
    ),
]


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
                self.send({"data": {"label": "local model fixture"}})
            elif "/model/" in self.path:
                requested = unquote(self.path).split("/model/", 1)[1].split("?", 1)[0]
                self.send({"data": next((row for row in MODELS if row["id"] == requested), model(requested, "Local model fixture"))})
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
        with tempfile.TemporaryDirectory(prefix="heycode-model-scope-") as folder:
            root = Path(folder)
            home, workspace = root / "home", root / "workspace"
            home.mkdir(); workspace.mkdir()
            settings = home / "settings.toml"
            settings.write_text("schema_version = 1\n")
            base = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(f'schema_version = 31\n[llm]\nprovider = "openrouter"\nmodel = "{MODEL["id"]}"\napi_key_env = "HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"\nbase_url = "{base}"\n')

            def start(phase):
                instance = Driver(binary=binary, home=home, workspace=workspace, base_url=base, output=output, phase=phase, columns=110, rows=42, timeout=30, model_id=None)
                instance.wait("shift+tab to cycle")
                return instance

            def capture(name):
                result["captures"].append(f"{driver.phase}-{name}")
                return driver.capture(name)

            def highlight(model_id):
                driver.command("/model", "this session only")
                driver.wait("source:")
                driver.wait(model_id)
                for _ in range(len(MODELS) + 1):
                    selected = [line for line in driver.screen.display if line.lstrip().startswith("❯ ") and model_id in line]
                    if selected: break
                    os.write(driver.tui.fd, b"\x1b[B"); driver.read(.2)
                else: raise AssertionError(f"Could not select {model_id}")

            def choose(model_id, scope, effort=None):
                highlight(model_id)
                driver.wait("←/→ to adjust")
                if effort:
                    # Select the requested value from the adapter-owned scale,
                    # without assuming where its advertised default sits.
                    for _ in range(10):
                        os.write(driver.tui.fd, b"\x1b[D")
                    driver.read(.3)
                    for _ in range(10):
                        if f"{effort} effort" in driver.read(): break
                        os.write(driver.tui.fd, b"\x1b[C")
                    driver.wait(f"{effort} effort")
                capture("select-" + model_id.replace("/", "-") + "-" + scope)
                os.write(driver.tui.fd, b"s" if scope == "session" else b"\r")
                driver.wait(f"Model set to {model_id}")
                receipt = capture("receipt-" + scope)
                if effort:
                    driver.command("/effort", "s for this session")
                    actual = capture("effective-" + effort + "-" + scope)
                    assert f"{effort}  current" in actual, actual
                    driver.escape()
                return receipt

            initial_settings = settings.read_bytes()
            driver = start("01")
            choose(MAIN_MODEL, "session", "high")
            assert settings.read_bytes() == initial_settings
            driver.close(); driver = None
            driver = start("02")
            driver.wait(MODEL["id"])
            capture("restart-original-model")
            choose(MAIN_MODEL, "default", "high")
            persisted = settings.read_bytes()
            assert ('model = "' + MAIN_MODEL + '"').encode() in persisted
            assert b'effort = "high"' in persisted
            choose(ADVISOR_MODEL, "session", "low")
            assert settings.read_bytes() == persisted
            driver.command("/model", "this session only")
            driver.wait("source:")
            os.write(driver.tui.fd, b"/s"); driver.read(.4)
            assert "/s" in driver.read(), driver.read()
            capture("search-letter-s")
            assert settings.read_bytes() == persisted
            driver.escape()
            driver.close(); driver = None
            driver = start("03")
            driver.wait(MAIN_MODEL)
            capture("restart-saved-model")
            driver.command("/effort", "s for this session")
            assert "high  current" in capture("restart-saved-effort")
            driver.escape()
            # A preview is never applied if the picker is cancelled.
            driver.command("/model", "this session only")
            driver.wait("high effort")
            os.write(driver.tui.fd, b"\x1b[D")
            driver.wait("low effort")
            capture("cancel-combined-preview")
            driver.escape()
            assert settings.read_bytes() == persisted
            driver.command("/effort", "s for this session")
            assert "high  current" in capture("cancel-retains-saved-effort")
            driver.escape()

            # The picker must show each highlighted model's own published
            # vocabulary, never the strict GLM route's max/high/low.
            highlight(GRADED_MODEL)
            driver.wait("medium effort (default)")
            graded = capture("graded-model-published-efforts")
            assert "max effort" not in graded, graded
            os.write(driver.tui.fd, b"\x1b[D")
            driver.wait("low effort")
            adjusted = capture("graded-model-adjusted-low")
            assert "max effort" not in adjusted, adjusted
            driver.escape()

            # A reasoning model that published no vocabulary offers no control
            # rather than borrowing another model's values.
            highlight(UNPUBLISHED_EFFORT_MODEL)
            unpublished = driver.wait("Effort unavailable for this model")
            capture("unpublished-effort-model")
            assert "max effort" not in unpublished, unpublished
            driver.escape()

            driver.close(); driver = None
            assert settings.read_bytes() == persisted
            assert not posts
            result["checks"] = {name: True for name in ["session_selection_no_write", "session_restart_restores_original", "enter_persists_default", "session_override_preserves_saved_model", "search_s_never_selects", "restart_restores_saved_model", "model_and_effort_commit_together", "session_effort_preserves_saved_default", "restart_restores_saved_effort", "cancel_discards_effort_preview", "published_vocabulary_per_model", "unpublished_vocabulary_offers_no_effort", "no_inference"]}
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
