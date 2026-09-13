#!/usr/bin/env python3
"""Command-line entry point for the Q10 benchmark harness."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

if __package__ in {None, ""}:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from benchmarks.runner import BudgetError, run_benchmarks


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--subject", choices=("fixture", "heycode"), default="fixture")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--budget-profile", default="fixture_ci")
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--seed", type=int, default=20260831)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--enforce-budgets", action="store_true")
    arguments = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    try:
        report = run_benchmarks(
            subject=arguments.subject,
            binary=arguments.binary,
            budget_profile=arguments.budget_profile,
            repetitions=arguments.repetitions,
            warmups=arguments.warmups,
            seed=arguments.seed,
            output=arguments.output.resolve(),
            repository=repository,
            enforce=arguments.enforce_budgets,
            baseline=arguments.baseline.resolve() if arguments.baseline else None,
        )
    except (BudgetError, OSError, ValueError) as error:
        print(f"q10 harness error: {type(error).__name__}", file=sys.stderr)
        return 2
    print(f"q10 {report['status']}: {len(report['metrics'])} metrics")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
