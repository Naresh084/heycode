#!/usr/bin/env python3
"""Command-line entry point for QSEC05 untrusted-content evaluations."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

if __package__ in {None, ""}:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from evals.security_runner import SecurityEvalError, run_security_eval


def main() -> int:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=repository / "evals/security/manifest.json")
    parser.add_argument("--subject", choices=("fixture", "heycode"), default="fixture")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--seed", type=int, default=20260831)
    parser.add_argument("--timeout-s", type=float, default=30.0)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--include-positive-control", action="store_true")
    arguments = parser.parse_args()
    try:
        report = run_security_eval(
            repository=repository,
            manifest_path=arguments.manifest.resolve(),
            subject=arguments.subject,
            binary=arguments.binary.resolve() if arguments.binary else None,
            seed=arguments.seed,
            timeout_s=arguments.timeout_s,
            output=arguments.output.resolve(),
            include_positive_control=arguments.include_positive_control,
        )
    except (OSError, SecurityEvalError, ValueError) as error:
        print(f"qsec05 harness error: {type(error).__name__}", file=sys.stderr)
        return 2
    print(f"qsec05 {report['status']}: {len(report['cases'])} denied-source cases")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
