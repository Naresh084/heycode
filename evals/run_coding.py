#!/usr/bin/env python3
"""Command-line entry point for Q11 matched coding-agent evaluations."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

if __package__ in {None, ""}:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from evals.coding_runner import EvalConfigError, run_coding_eval


def main() -> int:
    repository = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=repository / "evals/coding/manifest.json")
    parser.add_argument("--candidate", type=Path, default=repository / "evals/coding/agents/fixture-candidate.json")
    parser.add_argument("--reference", type=Path, default=repository / "evals/coding/agents/fixture-reference.json")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--seed", type=int, default=20260831)
    parser.add_argument("--bootstrap-resamples", type=int, default=20_000)
    parser.add_argument("--minimum-paired-observations", type=int, default=30)
    parser.add_argument("--noninferiority-margin", type=float, default=0.05)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--live", action="store_true")
    arguments = parser.parse_args()
    try:
        report = run_coding_eval(
            repository=repository,
            manifest_path=arguments.manifest.resolve(),
            candidate_path=arguments.candidate.resolve(),
            reference_path=arguments.reference.resolve(),
            repetitions=arguments.repetitions,
            base_seed=arguments.seed,
            bootstrap_resamples=arguments.bootstrap_resamples,
            minimum_paired_observations=arguments.minimum_paired_observations,
            noninferiority_margin=arguments.noninferiority_margin,
            output=arguments.output.resolve(),
            live=arguments.live,
        )
    except (EvalConfigError, OSError, ValueError) as error:
        print(f"q11 harness error: {type(error).__name__}", file=sys.stderr)
        return 2
    print(
        f"q11 {report['status']}: {len(report['trials'])} paired trials; "
        f"verdict={report['comparison']['verdict']}"
    )
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
