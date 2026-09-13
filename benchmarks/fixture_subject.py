#!/usr/bin/env python3
"""Deterministic black-box subject used to test the Q10 harness itself."""

from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("scenario")
    parser.add_argument("--fixture", type=Path)
    parser.add_argument("--count", type=int, default=1000)
    arguments = parser.parse_args()
    seed = int(os.environ["HEYCODE_EVAL_SEED"])

    if arguments.scenario == "startup_exit":
        return 0
    if arguments.scenario == "ttft_local":
        time.sleep(0.005 + (seed % 3) * 0.001)
        print("frame", flush=True)
        return 0
    if arguments.scenario == "replay_1000":
        if arguments.fixture is None:
            return 2
        expected = 0
        with arguments.fixture.open("rb") as source:
            for line in source:
                row = json.loads(line)
                if row["seq"] != expected:
                    return 3
                expected += 1
        return 0 if expected == arguments.count else 4
    if arguments.scenario == "render_flat":
        value = 0
        for index in range(arguments.count):
            value = (value * 1_103_515_245 + index + seed + 12_345) & 0xFFFFFFFF
        print("frame" if value or arguments.count else "empty", flush=True)
        return 0
    if arguments.scenario == "tool_registry_assembly":
        rows = {
            f"tool_{index}": {
                "type": "object",
                "properties": {"value": {"type": "integer"}},
            }
            for index in range(arguments.count)
        }
        json.dumps(rows, sort_keys=True, separators=(",", ":"))
        return 0
    return 5


if __name__ == "__main__":
    raise SystemExit(main())
