# Phase 2 live cache and task-quality experiment

2026-09-11. Native CLI with a fixed OpenRouter `anthropic/claude-sonnet-4` route and disposable fixture workspaces. Both arms used the same immutable binary. The baseline relay removed only message cache markers; all other inputs remained native. No authorization headers were retained.

| Mode | Provider requests including children | Checks correct | Reported cost USD | Cost per correct check USD | Warm cached-input share |
|---|---:|---:|---:|---:|---:|
| baseline | 25 | 7/7 | 1.17557100 | 0.16793871 | 0.0000% |
| cached | 26 | 7/7 | 0.83057835 | 0.11865405 | 99.7289% |

The seven checks were a cold no-tool answer, three short warm conversation turns, an actual file read, a long-file lookup, and five agents reading separate files. Five-agent success required five actual agent calls and five child sessions with a read, plus the exact returned fixture values. Each arm retained six distinct session archives. All 51 request snapshots stayed on the fixed model; configuration revision stayed 1 within each session. Both first requests have the same SHA-256 after removing cache markers: `a8563129f177cad89ab9c483f0153c2838aa063bc98f112a7c8f1ddd46b9722c`.

The cached mode read 38,993 of 39,099 billed input tokens from cache across three warm requests. Eligible unchanged-prefix tokens are not exposed by this provider evidence, so **warm reusable-prefix efficiency is unknown**. This ratio is not a universal 98% guarantee and is not a total-bill discount. Reported costs include all observed parent and child calls, including one extra request in the cached mode. This small fixed fixture set does not establish general task-quality superiority or quality preservation for lossy context changes. No such change was introduced.

The cache relay also retained upstream provider labels exactly as returned (`OpenAI` in these responses); those labels are not independently verified execution-location evidence. The selected application route/model is established by the request snapshots.

## Additional coverage and failures retained

- `cache-quality-parallel-explicit` passed both modes with two distinct `read` calls in one assistant message and both exact results. It is a separate cold two-file check, not folded into the main warm percentage.
- The first parallel supplement selected `read_many` in both modes and returned the correct values, but failed the explicit-two-call criterion. That result remains recorded as an instruction mismatch; the subsequent prompt specifically excluded other tools.
- `cache-quality-live-20260911T065536Z-a7337f` passed short turns/read but failed the long-file case in both modes. Captured diagnostics reproduced `tool-call response omitted required reasoning or reasoning_details`. OpenRouter Anthropic may emit a later tool call without another thinking block. The adapter now accepts that response while retaining authoritative native state and all reasoning actually returned; GLM/DeepSeek strict checks remain. The regression failed before the fix and all 458 inference tests passed after it.
- `cache-quality-live-20260911T070211Z-897b32` made zero provider calls: an in-progress MCP registration duplicated its contribution owner. That composition bug was repaired before the successful v3 run.

## Evidence

- Main raw requests, provider usage, and hashes (local-only evidence: `tmp/terminal-evidence/cache-quality-live-20260911T071248Z-57ec48/requests.json`)
- Per-check outcomes (local-only evidence: `tmp/terminal-evidence/cache-quality-live-20260911T071248Z-57ec48/outcomes.json`)
- Baseline context and request audit (local-only evidence: `tmp/terminal-evidence/cache-quality-live-20260911T071248Z-57ec48/baseline-context-audit.json`)
- Cached context and request audit (local-only evidence: `tmp/terminal-evidence/cache-quality-live-20260911T071248Z-57ec48/cached-context-audit.json`)
- Parallel-call supplement (local-only evidence: `tmp/terminal-evidence/cache-quality-parallel-explicit/summary.json`)
- Actual cache-counter terminal replay (local-only evidence: `tmp/terminal-evidence/live-cache-replay-20260911T070804Z-e29c7c/evidence.json`): an unmodified completed event prefix, source/prefix hashes, and zero inference POSTs; screenshot visually inspected. Startup catalog replies were local fixtures.
- [Controlled recovery audit](context-recovery-audit.md): 223 session tests plus production-composition retry/fallback tests; exact original history preserved and no duplicate original/compacted input.

## Remaining interpretation

The actual route produced cold/warm metrics, costs, long-result and child evidence. Retry, explicit fallback, configuration changes and compaction have controlled protocol/storage evidence; their live cache behavior is not established by this run. No claim of cheaper-than-Codex or higher-quality-than-Claude is made. Current official cache and reasoning semantics were checked against [OpenRouter prompt caching](https://openrouter.ai/docs/guides/best-practices/prompt-caching) and [reasoning preservation](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).
