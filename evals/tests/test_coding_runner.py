from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path

from evals.coding_runner import EvalConfigError, run_coding_eval
from quality.artifacts import read_artifact


REPOSITORY = Path(__file__).resolve().parents[2]
MANIFEST = REPOSITORY / "evals/coding/manifest.json"
CANDIDATE = REPOSITORY / "evals/coding/agents/fixture-candidate.json"
REFERENCE = REPOSITORY / "evals/coding/agents/fixture-reference.json"


class CodingEvalTests(unittest.TestCase):
    def test_matched_fixture_suite_grades_tasks_and_reports_intervals(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "q11.json"
            report = run_coding_eval(
                repository=REPOSITORY,
                manifest_path=MANIFEST,
                candidate_path=CANDIDATE,
                reference_path=REFERENCE,
                repetitions=2,
                base_seed=17,
                bootstrap_resamples=2_000,
                minimum_paired_observations=30,
                noninferiority_margin=0.05,
                output=output,
                live=False,
            )
            self.assertEqual(report, read_artifact(output))
            self.assertEqual(report["status"], "passed")
            self.assertEqual(len(report["trials"]), 6)
            self.assertTrue(
                all(
                    trial[agent]["success"]
                    for trial in report["trials"]
                    for agent in ("candidate", "reference")
                )
            )
            self.assertEqual(report["comparison"]["paired_total"], 6)
            self.assertEqual(report["comparison"]["verdict"], "insufficient_evidence")
            self.assertEqual(report["conditions"]["model_id"], "fixture/matched-model")

    def test_repeated_reports_keep_identical_seeds_and_success_statistics(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            reports = []
            for name in ("first", "second"):
                reports.append(
                    run_coding_eval(
                        repository=REPOSITORY,
                        manifest_path=MANIFEST,
                        candidate_path=CANDIDATE,
                        reference_path=REFERENCE,
                        repetitions=1,
                        base_seed=29,
                        bootstrap_resamples=2_000,
                        minimum_paired_observations=30,
                        noninferiority_margin=0.05,
                        output=root / f"{name}.json",
                        live=False,
                    )
                )
            self.assertEqual(
                [(row["task_id"], row["seed"]) for row in reports[0]["trials"]],
                [(row["task_id"], row["seed"]) for row in reports[1]["trials"]],
            )
            for first, second in zip(reports[0]["agents"], reports[1]["agents"], strict=True):
                for key in (
                    "agent_id",
                    "successes",
                    "total",
                    "success_rate",
                    "ci95_lower",
                    "ci95_upper",
                ):
                    self.assertEqual(first[key], second[key])
            self.assertEqual(reports[0]["comparison"], reports[1]["comparison"])

    def test_model_mismatch_fails_before_any_trial(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = json.loads(REFERENCE.read_text())
            config["model_id"] = "fixture/different-model"
            mismatched = root / "reference.json"
            mismatched.write_text(json.dumps(config))
            with self.assertRaises(EvalConfigError):
                run_coding_eval(
                    repository=REPOSITORY,
                    manifest_path=MANIFEST,
                    candidate_path=CANDIDATE,
                    reference_path=mismatched,
                    repetitions=1,
                    base_seed=1,
                    bootstrap_resamples=2_000,
                    minimum_paired_observations=30,
                    noninferiority_margin=0.05,
                    output=root / "report.json",
                    live=False,
                )

    def test_live_mode_requires_both_config_and_environment_gate(self) -> None:
        old = os.environ.pop("HEYCODE_EVAL_LIVE", None)
        try:
            with tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(EvalConfigError):
                    run_coding_eval(
                        repository=REPOSITORY,
                        manifest_path=MANIFEST,
                        candidate_path=CANDIDATE,
                        reference_path=REFERENCE,
                        repetitions=1,
                        base_seed=1,
                        bootstrap_resamples=2_000,
                        minimum_paired_observations=30,
                        noninferiority_margin=0.05,
                        output=Path(directory) / "report.json",
                        live=True,
                    )
        finally:
            if old is not None:
                os.environ["HEYCODE_EVAL_LIVE"] = old


if __name__ == "__main__":
    unittest.main()
