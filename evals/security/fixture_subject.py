#!/usr/bin/env python3
"""Deterministic unsafe-content trace fixture for the QSEC05 harness."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", choices=("web", "mcp", "lsp"), required=True)
    parser.add_argument("--session", type=Path, required=True)
    parser.add_argument("--marker", type=Path, required=True)
    parser.add_argument("--injection", type=Path, required=True)
    parser.add_argument("--approval", choices=("deny", "auto"), required=True)
    arguments = parser.parse_args()
    arguments.session.parent.mkdir(parents=True, exist_ok=True)
    source_tool = {
        "web": "web_fetch",
        "mcp": "mcp__fixture__read",
        "lsp": "lsp_diagnostics",
    }[arguments.source]
    rows = [
        {
            "v": 2,
            "seq": 0,
            "time_ms": 1,
            "kind": "tool/call",
            "data": {"turn": 0, "call_id": "qsec05_source", "name": source_tool, "args": {}},
        },
        {
            "v": 2,
            "seq": 1,
            "time_ms": 2,
            "kind": "tool/result",
            "data": {
                "call_id": "qsec05_source",
                "content": arguments.injection.read_text(encoding="utf-8"),
                "is_error": False,
                "untrusted_content": {"source": arguments.source},
            },
        },
        {
            "v": 2,
            "seq": 2,
            "time_ms": 3,
            "kind": "tool/call",
            "data": {"turn": 0, "call_id": "qsec05_attack", "name": "write", "args": {}},
        },
        {
            "v": 2,
            "seq": 3,
            "time_ms": 4,
            "kind": "tool/result",
            "data": {
                "call_id": "qsec05_attack",
                "content": "withheld",
                "is_error": arguments.approval == "deny"
            },
        },
    ]
    arguments.session.write_text(
        "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
        encoding="utf-8",
    )
    if arguments.approval == "auto":
        arguments.marker.write_text("control", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
