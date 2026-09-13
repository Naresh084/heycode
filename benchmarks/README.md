# Q10 black-box performance harness

`run.py` records five required product-facing metrics without importing private
crate internals:

| Metric | Fixture subject | heycode subject |
|---|---|---|
| `startup_exit` | process start/exit | CLI argument/help parse to its documented usage exit |
| `ttft_local` | delayed first byte | isolated `--fake` headless start to first output byte |
| `replay_1000` | parse 1,000 public v2 JSONL rows | resume the same public fixture and settle one fake turn |
| `render_flat` | build and emit a flat frame | PTY, screen-reader startup to first terminal bytes |
| `tool_registry_assembly` | assemble 1,000 schemas | isolated composition doctor over the actual default registry |

Every measured sample gets a fresh workspace and `HEYCODE_HOME`. Warmups are
excluded. The harness drains output without retaining it and owns process-tree
cleanup. The PTY probe asks the TUI to quit after the first frame and kills the
process group if it does not settle promptly.

`budgets.json` contains three explicit profiles:

- `fixture_ci` is an enforced harness self-test ceiling.
- `ci_debug` is an enforced hosted/debug regression smoke. Its generous
  ceilings catch hangs and gross regressions; passing it is not a release-speed
  claim.
- `release_reference` defines proposed absolute p95 targets (300 ms startup,
  500 ms local TTFT, 750 ms 1K replay, 250 ms first flat frame and 1,000 ms
  registry composition) plus a 10% relative threshold. It is intentionally
  rejected by `--enforce-budgets` until root records reference hardware and
  changes its evidence state.

Run deterministic fixture and product smokes:

```sh
python3 benchmarks/run.py --subject fixture --budget-profile fixture_ci \
  --repetitions 5 --warmups 1 --enforce-budgets --output /tmp/q10-fixture.json

python3 benchmarks/run.py --subject heycode --binary target/debug/heycode \
  --budget-profile ci_debug --repetitions 5 --warmups 1 \
  --enforce-budgets --output /tmp/q10-dshx.json
```

Use `--baseline /absolute/prior.json` to enforce the profile's relative p95
threshold against an actually observed, content-free baseline. Reports retain
p50/p95/p99, all measured samples, subject digest and closed settlements; they
retain no command, prompt, output, path, environment or host name.

