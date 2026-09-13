#!/usr/bin/env python3
"""Deterministic workspace mutator used only to prove the Q11 harness."""

from __future__ import annotations

import argparse
from pathlib import Path


REPAIR_SUM = """def add(left: int, right: int) -> int:\n    return left + right\n"""
INVENTORY = """def normalize_name(value: str) -> str:\n    return \" \".join(value.strip().split()).casefold()\n\n\ndef count_items(values: list[str]) -> dict[str, int]:\n    counts: dict[str, int] = {}\n    for value in values:\n        name = normalize_name(value)\n        counts[name] = counts.get(name, 0) + 1\n    return counts\n"""
REPORT = """from inventory import count_items\n\n\ndef render_report(values: list[str]) -> str:\n    counts = count_items(values)\n    return \"\\n\".join(f\"{name}: {counts[name]}\" for name in sorted(counts))\n"""
PARSER = """def parse_pairs(value: str) -> dict[str, str]:\n    pairs: dict[str, str] = {}\n    for field in value.split(\",\"):\n        key, separator, item = field.partition(\"=\")\n        if not separator:\n            raise ValueError(\"missing equals sign\")\n        normalized = key.strip()\n        if not normalized or normalized in pairs:\n            raise ValueError(\"invalid key\")\n        pairs[normalized] = item.strip()\n    return pairs\n"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--task", required=True)
    parser.add_argument("--seed", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--prompt", required=True)
    arguments = parser.parse_args()
    if arguments.task == "repair_sum":
        (arguments.workspace / "calculator.py").write_text(REPAIR_SUM, encoding="utf-8")
    elif arguments.task == "inventory_report":
        (arguments.workspace / "inventory.py").write_text(INVENTORY, encoding="utf-8")
        (arguments.workspace / "report.py").write_text(REPORT, encoding="utf-8")
    elif arguments.task == "parser_refactor":
        (arguments.workspace / "parser.py").write_text(PARSER, encoding="utf-8")
    else:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

