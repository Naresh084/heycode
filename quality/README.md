# Black-box quality group

This directory is the shared, dependency-free support layer for Q10, Q11 and
QSEC05. It deliberately imports no heycode crate and calls the product only as an
executable. A runner or workflow existing is not evidence that a budget,
provider comparison or security case passed.

The common boundary provides:

- a minimal environment with private `HOME`, `HEYCODE_HOME`, temporary and
  workspace roots;
- no ambient credential-shaped environment variables unless a live config
  names an exact variable and both live gates are present;
- process-group ownership, bounded output draining, deadlines and descendant
  cleanup;
- deterministic percentile, Wilson-score and paired-bootstrap calculations;
- schema-v1 result files that can represent only closed outcomes, counts,
  timings, short identifiers and digests.

Artifact validation rejects fields named like prompts, content, output,
commands, paths, environment, credentials, secrets, requests, responses or
reasoning. Child stdout/stderr is drained and counted in memory but never
retained. Result files are atomically published with mode `0600` on Unix.

Run the shared checks with:

```sh
python3 -m unittest discover -s quality/tests -v
python3 -m compileall -q quality benchmarks evals
python3 quality/validate_results.py /absolute/path/to/results/*.json
```

See [INTEGRATION.md](INTEGRATION.md) for owner-only hooks and current evidence
gaps.

