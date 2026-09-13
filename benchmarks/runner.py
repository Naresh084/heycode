"""Q10 reproducible black-box benchmark orchestration and budget evaluation."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from benchmarks.pty_probe import run_first_frame_probe
from quality.artifacts import read_artifact, write_artifact
from quality.process import ProcessLaunchError, ProcessResult, isolated_environment, run_discarded
from quality.stats import summarize

METRIC_IDS = (
    "startup_exit",
    "ttft_local",
    "replay_1000",
    "render_flat",
    "tool_registry_assembly",
)


class BudgetError(ValueError):
    """A budget document is malformed or cannot support the requested claim."""


def load_budget_profile(path: Path, profile: str, enforce: bool) -> dict[str, Any]:
    """Load one exact budget profile without interpreting missing values."""

    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BudgetError("budget document is invalid") from error
    if not isinstance(document, dict):
        raise BudgetError("budget document root must be an object")
    profiles = document.get("profiles")
    selected = profiles.get(profile) if isinstance(profiles, dict) else None
    if document.get("schema_version") != 1 or not isinstance(selected, dict):
        raise BudgetError("budget profile is absent or incompatible")
    if selected.get("state") not in {"enforced", "proposed"}:
        raise BudgetError("budget state is invalid")
    if enforce and selected["state"] != "enforced":
        raise BudgetError("a proposed budget cannot be reported as an enforced pass")
    metrics = selected.get("metrics")
    if not isinstance(metrics, dict) or set(metrics) != set(METRIC_IDS):
        raise BudgetError("budget metrics do not match the required Q10 set")
    for identifier in METRIC_IDS:
        row = metrics[identifier]
        if (
            not isinstance(row, dict)
            or not isinstance(row.get("p95_ms"), (int, float))
            or not isinstance(row.get("timeout_s"), (int, float))
            or row["p95_ms"] <= 0
            or row["timeout_s"] <= 0
        ):
            raise BudgetError("budget row is invalid")
    regression = selected.get("max_regression_percent")
    if not isinstance(regression, (int, float)) or not 0 <= regression <= 100:
        raise BudgetError("relative regression threshold is invalid")
    return selected


def generate_replay_fixture(path: Path, *, event_count: int) -> None:
    """Create a public-format v2 JSONL replay input with contiguous sequence."""

    if event_count < 1 or event_count > 100_000:
        raise ValueError("event_count is outside the benchmark fixture bound")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as output:
        for sequence in range(event_count):
            row = {
                "v": 2,
                "seq": sequence,
                "time_ms": 1_730_000_000_000 + sequence,
                "kind": "user/message",
                "data": {"text": "q10-replay-fixture"},
            }
            output.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
    if os.name != "nt":
        path.chmod(0o600)


def _digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def _platform_id() -> str:
    system = platform.system().lower().replace("darwin", "macos")
    machine = platform.machine().lower().replace("_", "-") or "unknown"
    return f"{system}-{machine}"


def _fixture_argv(repository: Path, identifier: str, replay: Path | None) -> list[str]:
    arguments = [sys.executable, str(repository / "benchmarks/fixture_subject.py"), identifier]
    if replay is not None:
        arguments.extend(["--fixture", str(replay), "--count", "1000"])
    elif identifier in {"render_flat", "tool_registry_assembly"}:
        arguments.extend(["--count", "1000"])
    return arguments


def _heycode_argv(binary: Path, identifier: str, replay: Path | None) -> list[str]:
    if identifier == "startup_exit":
        return [str(binary), "--help"]
    if identifier == "ttft_local":
        return [str(binary), "--restricted-workspace", "--fake", "run", "q10-ttft-fixture"]
    if identifier == "replay_1000":
        assert replay is not None
        return [
            str(binary),
            "--restricted-workspace",
            "--fake",
            "--resume",
            str(replay),
            "run",
            "q10-replay-settlement",
        ]
    if identifier == "render_flat":
        return [str(binary), "--screen-reader", "--restricted-workspace", "--fake"]
    if identifier == "tool_registry_assembly":
        return [str(binary), "--restricted-workspace", "doctor", "--composition", "--json"]
    raise ValueError("unknown metric")


def _run_once(
    *,
    subject: str,
    binary: Path | None,
    identifier: str,
    timeout_s: float,
    seed: int,
    repository: Path,
) -> tuple[float | None, ProcessResult | None, str]:
    with tempfile.TemporaryDirectory(prefix="heycode-q10-") as directory:
        root = Path(directory).resolve()
        workspace = root / "workspace"
        heycode_home = root / "heycode-home"
        environment = isolated_environment(root, heycode_home, workspace, seed)
        replay: Path | None = None
        if identifier == "replay_1000":
            replay = heycode_home / "sessions" / "q10-replay" / "session.jsonl"
            generate_replay_fixture(replay, event_count=1_000)
        if subject == "fixture":
            argv = _fixture_argv(repository, identifier, replay)
        else:
            assert binary is not None
            argv = _heycode_argv(binary, identifier, replay)
        try:
            if subject == "heycode" and identifier == "render_flat":
                result = run_first_frame_probe(
                    argv,
                    cwd=workspace,
                    env=environment,
                    timeout_s=timeout_s,
                )
            else:
                result = run_discarded(
                    argv,
                    cwd=workspace,
                    env=environment,
                    timeout_s=timeout_s,
                )
        except ProcessLaunchError:
            return None, None, "launch_failed"
        if identifier in {"ttft_local", "render_flat"}:
            value = result.first_output_ms
        else:
            value = result.duration_ms
        acceptable_probe = identifier == "render_flat" and result.settlement in {"exited", "probe_killed"}
        expected_help_exit = identifier == "startup_exit" and result.returncode == 1
        successful = (
            value is not None
            and not result.timed_out
            and not result.output_exceeded
            and (result.returncode == 0 or acceptable_probe or expected_help_exit)
        )
        return value if successful else None, result, result.settlement if successful else "failed"


def compare_to_baseline(
    candidate: dict[str, Any],
    baseline: dict[str, Any],
    *,
    max_regression_percent: float,
) -> dict[str, bool]:
    """Compare matched p95 metrics; absence or zero baseline fails closed."""

    baseline_rows = {
        row.get("id"): row for row in baseline.get("metrics", []) if isinstance(row, dict)
    }
    result: dict[str, bool] = {}
    for row in candidate.get("metrics", []):
        identifier = row.get("id")
        prior = baseline_rows.get(identifier)
        current_p95 = row.get("p95_ms")
        prior_p95 = prior.get("p95_ms") if isinstance(prior, dict) else None
        result[identifier] = bool(
            isinstance(current_p95, (int, float))
            and isinstance(prior_p95, (int, float))
            and prior_p95 > 0
            and current_p95 <= prior_p95 * (1.0 + max_regression_percent / 100.0)
        )
    return result


def run_benchmarks(
    *,
    subject: str,
    binary: Path | None,
    budget_profile: str,
    repetitions: int,
    warmups: int,
    seed: int,
    output: Path,
    repository: Path,
    enforce: bool,
    baseline: Path | None = None,
) -> dict[str, Any]:
    """Run Q10 metrics and write one strict content-free result."""

    if subject not in {"fixture", "heycode"} or repetitions < 1 or warmups < 0:
        raise ValueError("invalid benchmark request")
    if repetitions > 100 or warmups > 20:
        raise ValueError("benchmark repetition cap exceeded")
    if subject == "heycode":
        if binary is None or not binary.is_file():
            raise ValueError("a heycode binary is required")
        executable = binary.resolve()
    else:
        executable = repository / "benchmarks/fixture_subject.py"
    budget = load_budget_profile(repository / "benchmarks/budgets.json", budget_profile, enforce)
    metrics = []
    all_passed = True
    for metric_index, identifier in enumerate(METRIC_IDS):
        row_budget = budget["metrics"][identifier]
        samples: list[float] = []
        settlements: dict[str, int] = {}
        for run_index in range(warmups + repetitions):
            sample, _result, settlement = _run_once(
                subject=subject,
                binary=binary.resolve() if binary is not None else None,
                identifier=identifier,
                timeout_s=float(row_budget["timeout_s"]),
                seed=seed + metric_index * 10_000 + run_index,
                repository=repository,
            )
            if run_index < warmups:
                continue
            settlements[settlement] = settlements.get(settlement, 0) + 1
            if sample is not None:
                samples.append(sample)
        if samples:
            summary = summarize(samples)
            p95 = float(summary["p95"])
            passed = len(samples) == repetitions and p95 <= float(row_budget["p95_ms"])
            metric = {
                "id": identifier,
                "status": "passed" if passed else "failed",
                "samples_ms": [round(value, 6) for value in samples],
                "count": summary["count"],
                "mean_ms": summary["mean"],
                "p50_ms": summary["p50"],
                "p95_ms": summary["p95"],
                "p99_ms": summary["p99"],
                "budget_p95_ms": float(row_budget["p95_ms"]),
                "settlements": settlements,
            }
        else:
            passed = False
            metric = {
                "id": identifier,
                "status": "failed",
                "samples_ms": [],
                "count": 0,
                "budget_p95_ms": float(row_budget["p95_ms"]),
                "settlements": settlements,
            }
        all_passed = all_passed and passed
        metrics.append(metric)

    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": "q10_benchmark",
        "run_id": f"q10-{subject}-{seed}",
        "status": "passed" if all_passed else "failed",
        "subject": subject,
        "platform": _platform_id(),
        "seed": seed,
        "repetitions": repetitions,
        "warmups": warmups,
        "started_unix_ms": int(time.time() * 1000),
        "budget_profile": budget_profile,
        "budget_state": budget["state"],
        "host_class": budget["host_class"],
        "binary_sha256": _digest(executable),
        "metrics": metrics,
    }
    if baseline is not None:
        prior = read_artifact(baseline)
        relative = compare_to_baseline(
            report,
            prior,
            max_regression_percent=float(budget["max_regression_percent"]),
        )
        report["relative_budget_percent"] = float(budget["max_regression_percent"])
        report["relative_metrics"] = [
            {"id": identifier, "status": "passed" if passed else "failed"}
            for identifier, passed in sorted(relative.items())
        ]
        if not all(relative.values()):
            report["status"] = "failed"
    if budget["state"] == "proposed" and report["status"] == "passed":
        report["status"] = "inconclusive"
    write_artifact(output, report)
    return report
