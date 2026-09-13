from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from benchmarks.runner import (
    BudgetError,
    compare_to_baseline,
    generate_replay_fixture,
    load_budget_profile,
    run_benchmarks,
)
from quality.artifacts import read_artifact


REPOSITORY = Path(__file__).resolve().parents[2]


class BenchmarkRunnerTests(unittest.TestCase):
    def test_replay_fixture_is_contiguous_public_v2_jsonl(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.jsonl"
            generate_replay_fixture(path, event_count=1_000)
            rows = [json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual(len(rows), 1_000)
            self.assertEqual([row["seq"] for row in rows], list(range(1_000)))
            self.assertTrue(all(row["v"] == 2 for row in rows))
            self.assertTrue(all(row["kind"] == "user/message" for row in rows))

    def test_fixture_subject_records_all_required_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "q10.json"
            report = run_benchmarks(
                subject="fixture",
                binary=None,
                budget_profile="fixture_ci",
                repetitions=3,
                warmups=1,
                seed=17,
                output=output,
                repository=REPOSITORY,
                enforce=True,
            )
            self.assertEqual(report, read_artifact(output))
            self.assertEqual(report["status"], "passed")
            self.assertEqual(
                {metric["id"] for metric in report["metrics"]},
                {
                    "startup_exit",
                    "ttft_local",
                    "replay_1000",
                    "render_flat",
                    "tool_registry_assembly",
                },
            )
            self.assertTrue(all(len(metric["samples_ms"]) == 3 for metric in report["metrics"]))

    def test_proposed_release_budget_cannot_be_enforced_as_observed(self) -> None:
        with self.assertRaises(BudgetError):
            load_budget_profile(REPOSITORY / "benchmarks/budgets.json", "release_reference", True)

    def test_relative_comparison_fails_only_the_regressed_metric(self) -> None:
        baseline = {
            "metrics": [
                {"id": "startup_exit", "p95_ms": 100.0},
                {"id": "render_flat", "p95_ms": 100.0},
            ]
        }
        candidate = {
            "metrics": [
                {"id": "startup_exit", "p95_ms": 116.0},
                {"id": "render_flat", "p95_ms": 110.0},
            ]
        }
        result = compare_to_baseline(candidate, baseline, max_regression_percent=15.0)
        self.assertEqual(result, {"startup_exit": False, "render_flat": True})


if __name__ == "__main__":
    unittest.main()
