from __future__ import annotations

import unittest

from quality.stats import paired_bootstrap_interval, summarize, wilson_interval


class StatsTests(unittest.TestCase):
    def test_summary_uses_nearest_rank_for_tail_percentiles(self) -> None:
        summary = summarize([4.0, 1.0, 3.0, 2.0, 100.0])
        self.assertEqual(summary["count"], 5)
        self.assertEqual(summary["p50"], 3.0)
        self.assertEqual(summary["p95"], 100.0)
        self.assertEqual(summary["p99"], 100.0)

    def test_wilson_interval_is_bounded_and_non_degenerate(self) -> None:
        lower, upper = wilson_interval(8, 10)
        self.assertGreater(lower, 0.0)
        self.assertLess(lower, 0.8)
        self.assertGreater(upper, 0.8)
        self.assertLessEqual(upper, 1.0)

    def test_paired_bootstrap_is_reproducible_and_preserves_pairing(self) -> None:
        pairs = [(1, 0), (1, 0), (1, 1), (0, 0)]
        first = paired_bootstrap_interval(pairs, seed=17, resamples=2_000)
        second = paired_bootstrap_interval(pairs, seed=17, resamples=2_000)
        self.assertEqual(first, second)
        self.assertGreaterEqual(first[0], 0.0)
        self.assertGreater(first[1], 0.0)


if __name__ == "__main__":
    unittest.main()

