from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from quality.artifacts import ArtifactError, read_artifact, write_artifact


class ArtifactTests(unittest.TestCase):
    def test_content_free_report_round_trips(self) -> None:
        metric_ids = (
            "startup_exit",
            "ttft_local",
            "replay_1000",
            "render_flat",
            "tool_registry_assembly",
        )
        report = {
            "schema_version": 1,
            "kind": "q10_benchmark",
            "run_id": "fixture-17",
            "status": "passed",
            "subject": "fixture",
            "platform": "linux-x86-64",
            "seed": 17,
            "repetitions": 2,
            "warmups": 1,
            "started_unix_ms": 1,
            "budget_profile": "fixture_ci",
            "budget_state": "enforced",
            "host_class": "python_fixture",
            "binary_sha256": "a" * 64,
            "metrics": [
                {
                    "id": identifier,
                    "status": "passed",
                    "samples_ms": [1.0, 2.0],
                    "count": 2,
                    "mean_ms": 1.5,
                    "p50_ms": 1.0,
                    "p95_ms": 2.0,
                    "p99_ms": 2.0,
                    "budget_p95_ms": 10.0,
                    "settlements": {"exited": 2},
                }
                for identifier in metric_ids
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            write_artifact(path, report)
            self.assertEqual(read_artifact(path), report)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_content_bearing_keys_are_rejected_at_any_depth(self) -> None:
        report = {
            "schema_version": 1,
            "kind": "q11_coding_eval",
            "status": "passed",
            "trials": [{"task_id": "repair_sum", "stdout": "secret"}],
        }
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ArtifactError):
                write_artifact(Path(directory) / "report.json", report)

    def test_paths_and_free_form_strings_are_rejected(self) -> None:
        report = {
            "schema_version": 1,
            "kind": "qsec05_prompt_injection",
            "status": "failed",
            "workspace": "/private/tmp/source",
        }
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ArtifactError):
                write_artifact(Path(directory) / "report.json", report)

    def test_unknown_metadata_key_cannot_expand_the_artifact_surface(self) -> None:
        report = {
            "schema_version": 1,
            "kind": "q10_benchmark",
            "status": "failed",
            "note": "looks-safe-but-is-not-schema",
        }
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ArtifactError):
                write_artifact(Path(directory) / "report.json", report)


if __name__ == "__main__":
    unittest.main()
