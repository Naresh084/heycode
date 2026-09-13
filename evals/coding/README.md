# Q11 matched-model coding-agent evaluation

The suite currently contains three small deterministic tasks: a bug repair, a
two-file feature and a behavior-preserving refactor. Each trial copies a clean
fixture to a private workspace, gives the candidate or reference agent the same
prompt/model/permission/sandbox/timeout declaration, runs a deterministic
grader, verifies required files changed, and rejects changes outside the task's
allowlist. Candidate/reference order alternates by seed, while each receives an
independent clean workspace.

Reports include per-task success, closed exit/grader classes, change counts,
wall time and TTFT when observed. Aggregate success uses Wilson 95% intervals.
Candidate-minus-reference success uses a fixed-seed paired bootstrap over
identical task/seed pairs. A superiority or non-inferiority verdict is withheld
until the configured minimum paired count is met; the default is 30. “Beats” is
never inferred from means or anecdotes.

The generic process boundary cannot truthfully infer tool retries, token/cache
usage, cost, approvals or compaction from an arbitrary client. Those values are
absent, not zero. A future client adapter may add a separately validated
content-free trace contract; scraping free-form stdout would violate the result
boundary.

The checked-in candidate/reference configs intentionally use the same local
fixture model and deterministic mutator. They prove the runner and graders, not
coding-agent quality:

```sh
python3 evals/run_coding.py --repetitions 3 --seed 20260831 \
  --output /tmp/q11-fixture.json
```

Agent configs are schema-v1 JSON with exact fields: `agent_id`, `model_id`,
`permission_mode`, `sandbox_mode`, `timeout_s`, `live`, `pass_env`, and an argv
array. Supported placeholders are `{python}`, `{repository}`, `{workspace}`,
`{task_id}`, `{seed}`, `{model}`, and `{prompt}`. No shell is inserted.

For a real comparison, provide two reviewed configs with `live:true`, identical
conditions and explicit environment-name allowlists, then require both gates:

```sh
HEYCODE_EVAL_LIVE=1 python3 evals/run_coding.py --live \
  --candidate /absolute/candidate.json --reference /absolute/reference.json \
  --repetitions 10 --output /tmp/q11-live.json
```

The artifact stores definition SHA-256 digests but never argv, prompts, model
output, diffs, paths, environment names/values or credentials. A config's
condition declaration is auditable metadata; the operator must verify that an
external reference client's command actually honors it.
