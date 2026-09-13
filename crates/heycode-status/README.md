# heycode-status

Human-only effective status and diagnostics. Commands read live composed
services and never infer desired state from configuration.

## `/config`

`status_plugin_with_config(ConfigReport)` gives `/config` the effective
configuration with the file or flag each value came from, rendered by
`heycode-config`. The bare `status_plugin()` keeps the command and says the
surface was composed without a report. `heycode config show` prints the same
report headlessly.

## Context and usage

`/context` also reports the replay policy the newest durable request ran
under (`retry: 2 attempt(s) · definitive failures only`). A duplicate dispatch
is either policy or a bug, and only the recorded policy tells them apart; a
header written before the field existed reads as "not recorded" rather than
claiming a policy it never ran under.

When no durable request has stated the provider's window (a fresh session, a
fake provider), `/context` budgets against the agent's configured ceiling and
says `(configured window)` instead of `window unknown`.

Default `status-context` owns immediate `/context` and `/usage` independently
from the base status plugin. `/context` explains the Agent's latest complete
five-contributor envelope, including refusal fallback, exact/provider/local
estimate methods, uncounted reasons, durable context-window bounds, cached
published-price cost or Unknown, latest durable cache/context-edit facts, and
the live compaction strategy catalog.

`/usage` projects JSONL only: per-turn reported/lower-bound tokens, routes,
outcomes and price-derived cost when every required fact exists. Unknown usage,
route or pricing never becomes zero. Turn and detailed-response output are each
limited to 20 newest rows. Cache-aware cost requires an explicit non-overlapping
partition and every used price; otherwise it stays Unknown. U16/CMD07 are
complete through the restart-surviving `assistant/response-metadata` plane.

N06 adds durable tool attribution to the same command. Local exact,
provider-exact and provider-aggregate rows stay separate; only correlated
call/result ids produce success/error/unsettled outcomes. Aggregate rows retain
provider request counts without synthetic calls, and cost renders Unknown
unless the durable event carries validated published evidence.

## Retained health

Optional default plugin `health-history` publishes `HealthHistoryStore` and
owns `/health`. The composition root supplies its owner-home-relative path.
Each command runs the live doctor, commits a safe projection, then renders the
exact retained history.

The schema-v1 JSONL store:

- caps entries, exact serialized bytes, checks and labels on every read/write;
- retains recent unhealthy runs preferentially without weakening hard caps;
- preserves file order when clocks move backward;
- distinguishes a torn final append from a terminated corrupt row;
- refuses newer or foreign documents without overwriting them;
- screens all text on construction and deserialization;
- writes owner-only directories/files atomically on Unix.

## Focused verification

```sh
cargo fmt -p heycode-status -- --check
cargo clippy -p heycode-status --all-targets -- -D warnings
cargo test -p heycode-status --no-fail-fast
```
