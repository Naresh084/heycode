# heycode-live-artifact

Credential-proof metadata for real provider/runtime canaries.

Artifacts always retain a validated provider/model route, wall-clock instant,
closed pass/fail/skip outcome and optional measured latency. Provider content is
withheld by default; an explicit opt-in still passes the shared credential
screen, and credential-shaped notes are always rejected. URLs, headers,
credentials and arbitrary failure messages have no artifact field.

`LiveArtifact::is_fresh_at` is the continuous-lane gate. It accepts only an
artifact at or before the caller's clock and within the exact supplied age;
future timestamps and expired passes never satisfy freshness.

QLIVE01 uses this boundary for the scheduled OpenRouter GLM-5.3-Flash lane.
The artifact contains only route, instant, latency and closed outcome. Missing
process-scoped credentials record `Skipped(NoCredential)` and fail the job;
only a fresh `Passed` artifact may satisfy the live-evidence row.

```sh
cargo clippy -p heycode-live-artifact --all-targets -- -D warnings
cargo test -p heycode-live-artifact
```
