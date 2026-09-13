#!/usr/bin/env python3
"""Validate the production advisor picker without making an inference call.

The caller supplies an immutable heycode binary. The run uses a disposable home
and workspace plus an OpenRouter-shaped catalog server on 127.0.0.1. Only GET
catalog discovery is allowed; any POST is recorded as a contract failure.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
from typing import Any
from urllib.parse import unquote

from advisor_lifecycle_terminal_check import Driver, MODELS, model, sha256


def run(
    binary: Path,
    output: Path,
    *,
    columns: int,
    rows: int,
    timeout: float,
) -> dict[str, Any]:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    catalog_requests: list[str] = []
    inference_requests: list[dict[str, Any]] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def send_json(self, value: object, status: int = 200) -> None:
            data = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
            catalog_requests.append(self.path)
            if self.path.endswith("/key"):
                self.send_json({"data": {"label": "local advisor picker fixture"}})
                return
            if "/model/" in self.path:
                requested = unquote(self.path).split("/model/", 1)[1].split("?", 1)[0]
                selected = next(
                    (item for item in MODELS if item["id"] == requested),
                    model(requested, "Local on-demand catalog fixture"),
                )
                self.send_json({"data": selected})
                return
            self.send_json(
                {"data": MODELS, "total_count": len(MODELS), "links": {"next": None}}
            )

        def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            inference_requests.append(
                {"path": self.path, "request_bytes": len(raw), "loopback": True}
            )
            self.send_json({"error": {"message": "inference forbidden in picker test"}}, 500)

        def log_message(self, *_args: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    prior_key = os.environ.get("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE")
    os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = "local-only-dummy-key"
    result: dict[str, Any] = {
        "status": "blocked",
        "scope": "production advisor picker and status with localhost catalog only",
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "provider_host": "127.0.0.1",
        "external_provider_requests": 0,
        "normal_conversation_prompts": 0,
        "credential_source": "dummy loopback key only",
        "terminal_size": {"columns": columns, "rows": rows},
    }

    try:
        with tempfile.TemporaryDirectory(prefix="heycode-advisor-picker-") as folder:
            root = Path(folder)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "settings.toml").write_text("schema_version = 1\n")
            base_url = f"http://127.0.0.1:{server.server_port}/api/v1"
            (home / "config.toml").write_text(
                "schema_version = 31\n"
                "[llm]\n"
                'provider = "openrouter"\n'
                'model = "local/main-model"\n'
                'api_key_env = "HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"\n'
                f'base_url = "{base_url}"\n'
            )
            driver = Driver(
                binary=binary,
                home=home,
                workspace=workspace,
                base_url=base_url,
                output=output,
                phase="01",
                columns=columns,
                rows=rows,
                timeout=timeout,
            )
            try:
                driver.wait("shift+tab to cycle")
                driver.command("/advisor", "Enter to confirm")
                picker = driver.capture("picker")
                picker_lines = picker.splitlines()
                os.write(driver.tui.fd, b"local/advisor-model")
                driver.wait("Filter: local/advisor-model")
                filtered = driver.capture("filtered")
                os.write(driver.tui.fd, b"\r")
                driver.read(0.5)
                driver.command(
                    "/advisor status",
                    "enabled: native:openrouter · local/advisor-model",
                )
                status = driver.capture("status")
            finally:
                driver.close()

            heading_rows = [
                index
                for index, line in enumerate(picker_lines)
                if line.strip() == "Advisor"
            ]
            panel_text = "\n".join(
                picker_lines[max(heading_rows[-1] - 1, 0) :] if heading_rows else []
            )
            contracts = {
                "catalog_loaded": bool(catalog_requests),
                "all_catalog_models_visible": sum(
                    "OpenRouter —" in line for line in picker_lines
                )
                == len(MODELS),
                "uncached_advisor_model_visible": "local/advisor-model" in picker,
                "uncached_advisor_model_selectable": (
                    "Filter: local/advisor-model" in filtered
                    and "enabled: native:openrouter · local/advisor-model" in status
                ),
                "provider_qualified_rows": any(
                    "1. OpenRouter —" in line for line in picker_lines
                ),
                "disabled_default_selected": any(
                    "›" in line and "No advisor" in line and "✔ current" in line
                    for line in picker_lines
                ),
                "brief_explanation": "optional consultation" in panel_text,
                "confirm_cancel_footer": (
                    "Enter to confirm" in panel_text and "Esc to cancel" in panel_text
                ),
                "bottom_aligned": bool(heading_rows) and heading_rows[-1] > rows // 2,
                "unboxed": all(character not in panel_text for character in "┌┐└┘"),
                "technical_details_not_in_picker": all(
                    phrase not in panel_text
                    for phrase in ("Descendant requests", "live generation", "settings revision")
                ),
                "technical_details_in_status": (
                    "settings revision:" in status
                    and "live generation:" in status
                    and "descendant requests" in status
                ),
                "no_inference_requests": not inference_requests,
            }
            result.update(
                {
                    "status": "passed" if all(contracts.values()) else "gaps_observed",
                    "contract": contracts,
                    "catalog_requests": catalog_requests,
                    "provider_requests": inference_requests,
                    "captures": ["01-picker", "01-filtered", "01-status"],
                }
            )
    except Exception as error:
        result.update({"status": "blocked", "error": f"{type(error).__name__}: {error}"})
    finally:
        if prior_key is None:
            os.environ.pop("HEYCODE_ADVISOR_LIFECYCLE_FIXTURE", None)
        else:
            os.environ["HEYCODE_ADVISOR_LIFECYCLE_FIXTURE"] = prior_key
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=3)
        (output / "requests.json").write_text(json.dumps(inference_requests, indent=2))
        (output / "result.json").write_text(json.dumps(result, indent=2))

    print(json.dumps(result, indent=2))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--timeout", type=float, default=30)
    arguments = parser.parse_args()
    outcome = run(
        arguments.binary,
        arguments.output,
        columns=arguments.columns,
        rows=arguments.rows,
        timeout=arguments.timeout,
    )
    raise SystemExit(0 if outcome["status"] == "passed" else 1)
