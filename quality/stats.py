"""Small deterministic statistics helpers with explicit evidence semantics."""

from __future__ import annotations

import math
import random
import statistics
from collections.abc import Sequence


def _nearest_rank(sorted_values: Sequence[float], probability: float) -> float:
    if not sorted_values:
        raise ValueError("at least one observation is required")
    rank = max(1, math.ceil(probability * len(sorted_values)))
    return float(sorted_values[rank - 1])


def summarize(values: Sequence[float]) -> dict[str, float | int]:
    """Return deterministic count/mean/p50/p95/p99 statistics."""

    if not values or any(not math.isfinite(value) or value < 0 for value in values):
        raise ValueError("observations must be finite, non-negative, and nonempty")
    ordered = sorted(float(value) for value in values)
    return {
        "count": len(ordered),
        "mean": round(statistics.fmean(ordered), 6),
        "p50": round(_nearest_rank(ordered, 0.50), 6),
        "p95": round(_nearest_rank(ordered, 0.95), 6),
        "p99": round(_nearest_rank(ordered, 0.99), 6),
    }


def wilson_interval(successes: int, total: int, z: float = 1.959963984540054) -> tuple[float, float]:
    """Return a two-sided Wilson score interval for a Bernoulli proportion."""

    if total <= 0 or successes < 0 or successes > total or z <= 0:
        raise ValueError("invalid Wilson interval inputs")
    proportion = successes / total
    denominator = 1.0 + z * z / total
    center = (proportion + z * z / (2.0 * total)) / denominator
    radius = (
        z
        * math.sqrt(proportion * (1.0 - proportion) / total + z * z / (4.0 * total * total))
        / denominator
    )
    return round(max(0.0, center - radius), 6), round(min(1.0, center + radius), 6)


def paired_bootstrap_interval(
    pairs: Sequence[tuple[int, int]],
    *,
    seed: int,
    resamples: int = 20_000,
    confidence: float = 0.95,
) -> tuple[float, float]:
    """Bootstrap a paired candidate-minus-reference success-rate interval.

    Pairing is by identical task and seed. The fixed bootstrap seed makes a
    report reproducible without pretending the underlying model is deterministic.
    """

    if not pairs or resamples < 1_000 or not 0.5 < confidence < 1.0:
        raise ValueError("invalid bootstrap inputs")
    if any(candidate not in (0, 1) or reference not in (0, 1) for candidate, reference in pairs):
        raise ValueError("paired outcomes must be binary")
    generator = random.Random(seed)
    size = len(pairs)
    estimates = []
    for _ in range(resamples):
        difference = 0
        for _ in range(size):
            candidate, reference = pairs[generator.randrange(size)]
            difference += candidate - reference
        estimates.append(difference / size)
    estimates.sort()
    tail = (1.0 - confidence) / 2.0
    return (
        round(_nearest_rank(estimates, tail), 6),
        round(_nearest_rank(estimates, 1.0 - tail), 6),
    )

