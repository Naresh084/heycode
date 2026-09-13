# Evaluation suites

The Q11 and QSEC05 suites share the black-box isolation and content-free result
boundary in [`../quality`](../quality/README.md). Deterministic fixtures are the
default. Real providers or subscription runtimes are never selected implicitly.

- [`coding/README.md`](coding/README.md) describes matched coding-agent trials,
  deterministic grading and confidence reporting.
- [`security/README.md`](security/README.md) describes source-specific indirect
  prompt injection cases and the independent approval/mutation oracle.

Run all eval unit tests with:

```sh
python3 -m unittest discover -s evals/tests -v
```

