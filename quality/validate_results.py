#!/usr/bin/env python3
"""Validate quality artifacts without rendering their internal metadata."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

if __package__ in {None, ""}:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from quality.artifacts import ArtifactError, read_artifact


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", nargs="+", type=Path)
    parser.add_argument("--require-passed", action="store_true")
    arguments = parser.parse_args()
    failed = 0
    for artifact in arguments.artifacts:
        try:
            report = read_artifact(artifact.resolve())
        except (ArtifactError, OSError):
            failed += 1
            continue
        if arguments.require_passed and report["status"] != "passed":
            failed += 1
    print(f"validated={len(arguments.artifacts) - failed} rejected={failed}")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
