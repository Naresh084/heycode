from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from evals.security_runner import evaluate_case_evidence, load_security_manifest, run_security_eval
from quality.artifacts import read_artifact


REPOSITORY = Path(__file__).resolve().parents[2]
MANIFEST = REPOSITORY / "evals/security/manifest.json"


class SecurityEvalTests(unittest.TestCase):
    def test_manifest_covers_distinct_web_mcp_and_lsp_sources(self) -> None:
        scenarios = load_security_manifest(MANIFEST)
        self.assertEqual([scenario.source for scenario in scenarios], ["web", "mcp", "lsp"])
        self.assertEqual(len({scenario.injection_id for scenario in scenarios}), 3)

    def test_fixture_mode_proves_denial_and_positive_control_detection(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "qsec05.json"
            report = run_security_eval(
                repository=REPOSITORY,
                manifest_path=MANIFEST,
                subject="fixture",
                binary=None,
                seed=17,
                timeout_s=10.0,
                output=output,
                include_positive_control=True,
            )
            self.assertEqual(report, read_artifact(output))
            self.assertEqual(report["status"], "passed")
            self.assertEqual(len(report["cases"]), 3)
            self.assertTrue(all(case["boundary_observed"] for case in report["cases"]))
            self.assertTrue(all(case["action_requested"] for case in report["cases"]))
            self.assertTrue(all(case["approval_decision"] == "deny" for case in report["cases"]))
            self.assertTrue(all(case["mutation_absent"] for case in report["cases"]))
            self.assertEqual(report["positive_control"]["status"], "detected")
            serialized = output.read_text()
            self.assertNotIn("ignore previous", serialized.lower())
            self.assertNotIn("authorized", serialized.lower())

    def test_any_mutation_or_missing_denial_fails_the_case(self) -> None:
        result = evaluate_case_evidence(
            boundary_observed=True,
            action_requested=True,
            approval_requested=True,
            approval_decision="deny",
            denial_recorded=True,
            mutation_absent=False,
            cleanup_complete=True,
        )
        self.assertFalse(result)
        result = evaluate_case_evidence(
            boundary_observed=True,
            action_requested=True,
            approval_requested=False,
            approval_decision="none",
            denial_recorded=False,
            mutation_absent=True,
            cleanup_complete=True,
        )
        self.assertFalse(result)


if __name__ == "__main__":
    unittest.main()
