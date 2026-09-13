"""Q11 matched-condition coding-agent runner with deterministic grading."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from quality.artifacts import write_artifact
from quality.process import ProcessLaunchError, isolated_environment, run_discarded
from quality.stats import paired_bootstrap_interval, summarize, wilson_interval

MAX_CONFIG_BYTES = 256 * 1024
MAX_PROMPT_BYTES = 32 * 1024
MAX_FIXTURE_BYTES = 8 * 1024 * 1024
MAX_FIXTURE_FILES = 512
_PLACEHOLDER = re.compile(r"\{[a-z_]+\}")
_KNOWN_PLACEHOLDERS = {
    "{model}",
    "{prompt}",
    "{python}",
    "{repository}",
    "{seed}",
    "{task_id}",
    "{workspace}",
}


class EvalConfigError(ValueError):
    """An eval definition is unsafe, unmatched, or incompatible."""


@dataclass(frozen=True)
class AgentConfig:
    agent_id: str
    model_id: str
    permission_mode: str
    sandbox_mode: str
    timeout_s: float
    live: bool
    pass_env: tuple[str, ...]
    argv: tuple[str, ...]


@dataclass(frozen=True)
class TaskConfig:
    task_id: str
    category: str
    prompt_path: Path
    fixture_dir: Path
    allowed_changes: frozenset[str]
    required_changes: frozenset[str]
    grader: tuple[str, ...]
    grader_timeout_s: float


def _bounded_json(path: Path) -> dict[str, Any]:
    try:
        if path.stat().st_size > MAX_CONFIG_BYTES or path.is_symlink() or not path.is_file():
            raise EvalConfigError("definition is not a bounded regular file")
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EvalConfigError("definition is invalid") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise EvalConfigError("definition schema is incompatible")
    return value


def _safe_id(value: Any) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", value):
        raise EvalConfigError("identifier is invalid")
    return value


def _safe_relative(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value.encode()) > 240:
        raise EvalConfigError("relative path is invalid")
    path = Path(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise EvalConfigError("relative path escapes its fixture")
    return path.as_posix()


def _resolve_inside(base: Path, relative: Any, *, directory: bool) -> Path:
    safe = _safe_relative(relative)
    raw = base / safe
    current = base
    for part in Path(safe).parts:
        current /= part
        if current.is_symlink():
            raise EvalConfigError("definition path cannot traverse a symlink")
    candidate = raw.resolve(strict=True)
    if not candidate.is_relative_to(base.resolve()):
        raise EvalConfigError("definition path escapes its root")
    if candidate.is_symlink() or (directory and not candidate.is_dir()) or (not directory and not candidate.is_file()):
        raise EvalConfigError("definition path has the wrong type")
    return candidate


def load_agent_config(path: Path) -> AgentConfig:
    """Load one bounded argv-only agent definition."""

    value = _bounded_json(path)
    required = {
        "schema_version",
        "agent_id",
        "model_id",
        "permission_mode",
        "sandbox_mode",
        "timeout_s",
        "live",
        "pass_env",
        "argv",
    }
    if set(value) != required:
        raise EvalConfigError("agent definition fields are not exact")
    agent_id = _safe_id(value["agent_id"])
    model_id = value["model_id"]
    if not isinstance(model_id, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:/-]{0,127}", model_id):
        raise EvalConfigError("model id is invalid")
    permission = value["permission_mode"]
    sandbox = value["sandbox_mode"]
    if permission not in {"auto", "ask", "deny"} or sandbox not in {"off", "readonly", "workspace"}:
        raise EvalConfigError("permission or sandbox mode is invalid")
    timeout = value["timeout_s"]
    if not isinstance(timeout, (int, float)) or not 1 <= timeout <= 3600:
        raise EvalConfigError("agent timeout is invalid")
    live = value["live"]
    pass_env = value["pass_env"]
    argv = value["argv"]
    if not isinstance(live, bool) or not isinstance(pass_env, list) or not isinstance(argv, list):
        raise EvalConfigError("agent execution fields are invalid")
    if len(pass_env) > 16 or any(not isinstance(name, str) or not re.fullmatch(r"[A-Z][A-Z0-9_]{0,63}", name) for name in pass_env):
        raise EvalConfigError("inherited environment names are invalid")
    if len(set(pass_env)) != len(pass_env):
        raise EvalConfigError("inherited environment names must be unique")
    if len(argv) < 1 or len(argv) > 64 or any(not isinstance(item, str) or not item or len(item.encode()) > 65_536 for item in argv):
        raise EvalConfigError("agent argv is invalid")
    placeholders = set().union(*(_PLACEHOLDER.findall(item) for item in argv))
    if not placeholders <= _KNOWN_PLACEHOLDERS:
        raise EvalConfigError("agent argv contains an unknown placeholder")
    return AgentConfig(
        agent_id=agent_id,
        model_id=model_id,
        permission_mode=permission,
        sandbox_mode=sandbox,
        timeout_s=float(timeout),
        live=live,
        pass_env=tuple(pass_env),
        argv=tuple(argv),
    )


def load_manifest(path: Path) -> tuple[str, list[TaskConfig]]:
    """Load the deterministic task/grader manifest."""

    value = _bounded_json(path)
    if set(value) != {"schema_version", "suite_id", "tasks"}:
        raise EvalConfigError("manifest fields are not exact")
    suite_id = _safe_id(value["suite_id"])
    tasks = value["tasks"]
    if not isinstance(tasks, list) or not 1 <= len(tasks) <= 64:
        raise EvalConfigError("manifest task count is invalid")
    base = path.parent.resolve()
    loaded: list[TaskConfig] = []
    seen: set[str] = set()
    for row in tasks:
        if not isinstance(row, dict) or set(row) != {
            "id",
            "category",
            "prompt_file",
            "fixture_dir",
            "allowed_changes",
            "required_changes",
            "grader",
            "grader_timeout_s",
        }:
            raise EvalConfigError("task fields are not exact")
        task_id = _safe_id(row["id"])
        category = _safe_id(row["category"])
        if task_id in seen:
            raise EvalConfigError("task ids must be unique")
        seen.add(task_id)
        allowed_raw = row["allowed_changes"]
        required_raw = row["required_changes"]
        grader = row["grader"]
        timeout = row["grader_timeout_s"]
        if (
            not isinstance(allowed_raw, list)
            or not isinstance(required_raw, list)
            or not isinstance(grader, list)
            or not grader
            or not isinstance(timeout, (int, float))
            or not 1 <= timeout <= 300
        ):
            raise EvalConfigError("task grading fields are invalid")
        allowed = frozenset(_safe_relative(item) for item in allowed_raw)
        required = frozenset(_safe_relative(item) for item in required_raw)
        if not required <= allowed:
            raise EvalConfigError("required changes must be allowed")
        if any(not isinstance(item, str) or not item for item in grader):
            raise EvalConfigError("grader argv is invalid")
        placeholders = set().union(*(_PLACEHOLDER.findall(item) for item in grader))
        if not placeholders <= {"{python}", "{workspace}"}:
            raise EvalConfigError("grader argv contains an unknown placeholder")
        prompt_path = _resolve_inside(base, row["prompt_file"], directory=False)
        if prompt_path.stat().st_size > MAX_PROMPT_BYTES:
            raise EvalConfigError("task prompt exceeds the byte cap")
        if "\x00" in prompt_path.read_text(encoding="utf-8"):
            raise EvalConfigError("task prompt contains a null byte")
        fixture_dir = _resolve_inside(base, row["fixture_dir"], directory=True)
        loaded.append(
            TaskConfig(
                task_id=task_id,
                category=category,
                prompt_path=prompt_path,
                fixture_dir=fixture_dir,
                allowed_changes=allowed,
                required_changes=required,
                grader=tuple(grader),
                grader_timeout_s=float(timeout),
            )
        )
    return suite_id, loaded


def _walk_files(root: Path) -> list[Path]:
    files: list[Path] = []
    total = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise EvalConfigError("fixture contains a symlink")
        if path.is_dir():
            continue
        relative = path.relative_to(root)
        if "__pycache__" in relative.parts or path.suffix == ".pyc":
            continue
        if not path.is_file():
            raise EvalConfigError("fixture contains a non-file entry")
        total += path.stat().st_size
        files.append(path)
        if len(files) > MAX_FIXTURE_FILES or total > MAX_FIXTURE_BYTES:
            raise EvalConfigError("fixture exceeds the file or byte cap")
    return files


def _snapshot(root: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for path in _walk_files(root):
        result[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def _file_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _substitute(template: tuple[str, ...], values: dict[str, str]) -> list[str]:
    rendered = []
    for item in template:
        current = item
        for name, value in values.items():
            current = current.replace("{" + name + "}", value)
        rendered.append(current)
    return rendered


def _exit_class(returncode: int | None, timed_out: bool, exceeded: bool) -> str:
    if timed_out:
        return "timeout"
    if exceeded:
        return "output_limit"
    if returncode == 0:
        return "zero"
    return "nonzero"


def _run_agent_trial(
    *,
    repository: Path,
    task: TaskConfig,
    config: AgentConfig,
    seed: int,
) -> tuple[dict[str, Any], bool]:
    with tempfile.TemporaryDirectory(prefix="heycode-q11-") as directory:
        root = Path(directory).resolve()
        workspace = root / "workspace"
        _walk_files(task.fixture_dir)
        shutil.copytree(task.fixture_dir, workspace)
        before = _snapshot(workspace)
        prompt = task.prompt_path.read_text(encoding="utf-8")
        values = {
            "python": sys.executable,
            "repository": str(repository.resolve()),
            "workspace": str(workspace),
            "task_id": task.task_id,
            "seed": str(seed),
            "model": config.model_id,
            "prompt": prompt,
        }
        argv = _substitute(config.argv, values)
        environment = isolated_environment(
            root,
            root / "heycode-home",
            workspace,
            seed,
            pass_env=config.pass_env,
            extra={
                "HEYCODE_EVAL_TASK_ID": task.task_id,
                "HEYCODE_EVAL_MODEL_ID": config.model_id,
                "HEYCODE_EVAL_PERMISSION_MODE": config.permission_mode,
                "HEYCODE_EVAL_SANDBOX_MODE": config.sandbox_mode,
                "PYTHONDONTWRITEBYTECODE": "1",
            },
        )
        harness_ok = True
        try:
            agent = run_discarded(
                argv,
                cwd=workspace,
                env=environment,
                timeout_s=config.timeout_s,
            )
            agent_class = _exit_class(agent.returncode, agent.timed_out, agent.output_exceeded)
            duration_ms = agent.duration_ms
            ttft_ms = agent.first_output_ms
            settlement = agent.settlement
        except ProcessLaunchError:
            agent_class = "launch_failed"
            duration_ms = 0.0
            ttft_ms = None
            settlement = "launch_failed"
            harness_ok = False

        after = _snapshot(workspace)
        changed = {name for name in set(before) | set(after) if before.get(name) != after.get(name)}
        unexpected = changed - task.allowed_changes
        required_present = task.required_changes <= changed
        grader_values = {"python": sys.executable, "workspace": str(workspace)}
        grader_argv = _substitute(task.grader, grader_values)
        try:
            grader = run_discarded(
                grader_argv,
                cwd=workspace,
                env=environment,
                timeout_s=task.grader_timeout_s,
            )
            grader_state = (
                "passed"
                if grader.returncode == 0 and not grader.timed_out and not grader.output_exceeded
                else "failed"
            )
        except ProcessLaunchError:
            grader_state = "launch_failed"
            harness_ok = False
        success = (
            agent_class == "zero"
            and grader_state == "passed"
            and not unexpected
            and required_present
        )
        return (
            {
                "success": success,
                "agent_exit_class": agent_class,
                "grader_state": grader_state,
                "settlement": settlement,
                "duration_ms": duration_ms,
                "ttft_ms": ttft_ms,
                "changed_count": len(changed),
                "unexpected_count": len(unexpected),
                "required_count": len(task.required_changes),
            },
            harness_ok,
        )


def _agent_summary(agent_id: str, rows: list[dict[str, Any]]) -> dict[str, Any]:
    successes = sum(1 for row in rows if row["success"])
    total = len(rows)
    lower, upper = wilson_interval(successes, total)
    durations = summarize([float(row["duration_ms"]) for row in rows])
    observed_ttft = [float(row["ttft_ms"]) for row in rows if row["ttft_ms"] is not None]
    summary: dict[str, Any] = {
        "agent_id": agent_id,
        "successes": successes,
        "total": total,
        "success_rate": round(successes / total, 6),
        "ci95_lower": lower,
        "ci95_upper": upper,
        "duration_mean_ms": durations["mean"],
        "duration_p50_ms": durations["p50"],
        "duration_p95_ms": durations["p95"],
        "ttft_observed_count": len(observed_ttft),
    }
    if observed_ttft:
        ttft = summarize(observed_ttft)
        summary["ttft_p50_ms"] = ttft["p50"]
        summary["ttft_p95_ms"] = ttft["p95"]
    return summary


def _verdict(
    interval: tuple[float, float],
    *,
    paired_total: int,
    minimum: int,
    margin: float,
) -> str:
    if paired_total < minimum:
        return "insufficient_evidence"
    lower, upper = interval
    if lower > 0:
        return "superior"
    if upper < -margin:
        return "inferior"
    if lower >= -margin:
        return "noninferior"
    return "inconclusive"


def run_coding_eval(
    *,
    repository: Path,
    manifest_path: Path,
    candidate_path: Path,
    reference_path: Path,
    repetitions: int,
    base_seed: int,
    bootstrap_resamples: int,
    minimum_paired_observations: int,
    noninferiority_margin: float,
    output: Path,
    live: bool,
) -> dict[str, Any]:
    """Run matched candidate/reference trials and emit a content-free report."""

    if not 1 <= repetitions <= 100:
        raise EvalConfigError("repetitions are outside the bound")
    if not 1_000 <= bootstrap_resamples <= 100_000:
        raise EvalConfigError("bootstrap resamples are outside the bound")
    if not 1 <= minimum_paired_observations <= 10_000 or not 0 <= noninferiority_margin <= 0.5:
        raise EvalConfigError("comparison policy is invalid")
    suite_id, tasks = load_manifest(manifest_path)
    candidate = load_agent_config(candidate_path)
    reference = load_agent_config(reference_path)
    matched = (
        candidate.model_id == reference.model_id
        and candidate.permission_mode == reference.permission_mode
        and candidate.sandbox_mode == reference.sandbox_mode
        and candidate.timeout_s == reference.timeout_s
        and candidate.live == reference.live
    )
    if not matched:
        raise EvalConfigError("candidate and reference conditions are not matched")
    if live:
        if os.environ.get("HEYCODE_EVAL_LIVE") != "1" or not candidate.live:
            raise EvalConfigError("live comparisons require both explicit gates")
    elif candidate.live or candidate.pass_env or reference.pass_env:
        raise EvalConfigError("deterministic mode cannot inherit live environment")

    trials: list[dict[str, Any]] = []
    candidate_rows: list[dict[str, Any]] = []
    reference_rows: list[dict[str, Any]] = []
    harness_ok = True
    for repetition in range(repetitions):
        for task_index, task in enumerate(tasks):
            seed = base_seed + repetition * len(tasks) + task_index
            if seed % 2:
                reference_result, reference_ok = _run_agent_trial(
                    repository=repository, task=task, config=reference, seed=seed
                )
                candidate_result, candidate_ok = _run_agent_trial(
                    repository=repository, task=task, config=candidate, seed=seed
                )
            else:
                candidate_result, candidate_ok = _run_agent_trial(
                    repository=repository, task=task, config=candidate, seed=seed
                )
                reference_result, reference_ok = _run_agent_trial(
                    repository=repository, task=task, config=reference, seed=seed
                )
            harness_ok = harness_ok and candidate_ok and reference_ok
            candidate_rows.append(candidate_result)
            reference_rows.append(reference_result)
            trials.append(
                {
                    "task_id": task.task_id,
                    "category": task.category,
                    "seed": seed,
                    "candidate": candidate_result,
                    "reference": reference_result,
                }
            )

    pairs = [
        (int(candidate_row["success"]), int(reference_row["success"]))
        for candidate_row, reference_row in zip(candidate_rows, reference_rows, strict=True)
    ]
    interval = paired_bootstrap_interval(
        pairs,
        seed=base_seed ^ 0x511,
        resamples=bootstrap_resamples,
    )
    if live:
        harness_ok = harness_ok and all(
            row["agent_exit_class"] == "zero" for row in candidate_rows + reference_rows
        )
    difference = sum(candidate_value - reference_value for candidate_value, reference_value in pairs) / len(pairs)
    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": "q11_coding_eval",
        "run_id": f"q11-{base_seed}-{repetitions}",
        "status": "passed" if harness_ok else "failed",
        "suite_id": suite_id,
        "live": live,
        "base_seed": base_seed,
        "repetitions": repetitions,
        "completed_unix_ms": int(time.time() * 1000),
        "conditions": {
            "model_id": candidate.model_id,
            "permission_mode": candidate.permission_mode,
            "sandbox_mode": candidate.sandbox_mode,
            "timeout_s": candidate.timeout_s,
        },
        "definitions": {
            "manifest_sha256": _file_digest(manifest_path),
            "candidate_config_sha256": _file_digest(candidate_path),
            "reference_config_sha256": _file_digest(reference_path),
        },
        "trials": trials,
        "agents": [
            _agent_summary(candidate.agent_id, candidate_rows),
            _agent_summary(reference.agent_id, reference_rows),
        ],
        "comparison": {
            "paired_total": len(pairs),
            "candidate_minus_reference": round(difference, 6),
            "ci95_lower": interval[0],
            "ci95_upper": interval[1],
            "bootstrap_resamples": bootstrap_resamples,
            "minimum_paired_observations": minimum_paired_observations,
            "noninferiority_margin": noninferiority_margin,
            "verdict": _verdict(
                interval,
                paired_total=len(pairs),
                minimum=minimum_paired_observations,
                margin=noninferiority_margin,
            ),
        },
    }
    write_artifact(output, report)
    return report
