# Phase 2 compact-focus completion audit

Date: 2026-09-11

## Outcome

`/compact` now accepts an optional human summary focus while retaining every
previous dshx form. Focus is implemented only for `portable-summary`; native
and prune strategies fail explicitly rather than pretending to honor it.

This is a local implementation and controlled-validation result. It does not
claim provider-summary quality from the fake-provider terminal run, and no paid
or external provider was contacted.

## Source contract

The current Claude Code command documentation describes
`/compact [instructions]` and says the optional instructions focus the summary.
Its example is `/compact focus on the API changes`:
<https://code.claude.com/docs/en/commands>.

dshx retains its existing strategy and keep controls and extends them with the
same natural focus affordance:

| Form | Meaning |
| --- | --- |
| `/compact` | Portable summary, default keep, no focus |
| `/compact 4` | Portable summary, keep four, no focus |
| `/compact portable-summary 4` | Named strategy and keep, no focus |
| `/compact focus on API changes` | Portable/default keep with natural focus |
| `/compact 4 -- focus on API changes` | Portable/keep four with delimited focus |
| `/compact portable-summary 4 -- focus on API changes` | Fully structural form |
| `/compact list` | Exact strategy listing form |

Numeric-looking keep values remain structural: zero, negative, and overflow
values are errors. An explicit `--` with no following text is an error. Unknown
nonnumeric text is interpreted as natural focus, not an invented strategy id.

## Implementation boundaries

- `Agent::compact` and `CompactionRegistry::compact` remain source-compatible
  no-focus wrappers. New `compact_with_focus` APIs carry optional focus.
- The strategy trait has a default focus-aware entrypoint that rejects focus.
  `PortableCompaction` is the only strategy that opts in.
- Focus is trimmed, must contain 1 through 4,096 UTF-8 bytes, and is validated
  before provider work or durable mutation.
- Every focused hierarchy request carries the focus in a user-role message as
  a JSON string. The system instruction preserves the original retention
  requirements and says focus cannot justify invention or dropping critical
  unfinished work.
- The focused framing delta is subtracted from the existing 24,000-byte
  summarization envelope. No-focus requests retain their exact prior system and
  user messages.
- Compaction remains append-only. Provider failure and caller cancellation do
  not append a settlement or alter the existing journal.

Primary implementation and tests:

- `crates/heycode-agent/src/commands.rs`
- `crates/heycode-agent/src/compaction_registry.rs`
- `crates/heycode-agent/src/agent.rs`
- `crates/heycode-agent/tests/it/compact.rs`
- `crates/heycode-tui/src/app.rs`
- `crates/heycode-tui/src/render.rs`

## Automated verification

`tmp/terminal-evidence/compact-focus-agent-tests.log` records:

- 127 unit tests passed;
- 320 integration tests passed;
- one unrelated, explicitly opt-in two-minute background-terminal test ignored;
- focused parser coverage for legacy, natural, delimited, and invalid forms;
- exact no-focus request compatibility;
- focus repeated at every hierarchical summary level within the unchanged
  input envelope;
- continuation projection excludes folded originals while the raw journal
  retains them;
- failure and cancellation preserve byte-exact journal contents; and
- invalid or unsupported focus dispatches no provider request.

`tmp/terminal-evidence/compact-focus-agent-clippy.log` records a passing strict
`cargo clippy -p dshx-agent --all-targets -- -D warnings` run. A focused
`git diff --check` also passed.

## Native terminal and restart verification

`scripts/compaction_focus_terminal_check.py` drove the immutable binary
`tmp/cli-snapshots/373bb8ce5429e5d0/dshx` (SHA-256
`373bb8ce5429e5d0a63d555511546b2726928ad9b193e8126a96f111483ce806`)
through an actual PTY using `--fake --no-background --trust-workspace`, a
disposable `DSHX_HOME`, and a disposable workspace.

The passing result is
`tmp/terminal-evidence/compact-focus-pty-20260911T104117Z-bc3319/result.json`. It records:

- three original turns;
- `/compact list` with all three composed strategies and no journal mutation;
- natural `/compact Prioritize API decisions, cancellation, and exact
  filenames.`;
- exactly one `compaction/applied` event;
- continuation before restart;
- exact-path session resume and continuation after restart;
- every original and continuation prompt retained exactly once;
- a byte-exact pre-compaction prefix in the final append-only journal; and
- zero external provider requests and zero durable transport request headers.

Inspectible artifacts include `events.json`, `session.jsonl`, `terminal.ansi`,
and nine text/PNG captures in `tmp/terminal-evidence/compact-focus-pty-20260911T104117Z-bc3319/`.

## Held-provider native state verification

`scripts/compaction_states_terminal_check.py` exercised the immutable binary
`tmp/cli-snapshots/25526dce041310f5/dshx` (SHA-256
`25526dce041310f5ccf263a406ef0717afeed43616a343ecd6dc8a278fce5010`)
through the production OpenRouter adapter against a deterministic HTTP/SSE
fixture bound to `127.0.0.1`. Each completion, failure, and cancellation
scenario used a fresh disposable `DSHX_HOME` and workspace and created three
synthetic turns before issuing the focused compact command. The passing
manifest is `tmp/terminal-evidence/dshx-compact-states-20260911T114552Z-3372e9/result.json`.

| Outcome | Native PTY observation | Durable or transport observation |
| --- | --- | --- |
| Working | `Compacting conversation… (Esc to cancel)` remains visible while the provider response is held | The exact focus and folded synthetic marker are present in the loopback request; journal prefix is unchanged |
| Complete | Collapsed `Compacted (ctrl+o to see full summary)` receipt; Ctrl+O reveals and collapses the complete retained summary; a real SGR mouse click expands it and a second click collapses it | Exactly one `compaction/applied` event; seeded journal remains a byte-exact prefix |
| Failure | Explicit normalized `compaction failed: provider rejected the request` receipt | No compaction boundary and the journal remains byte-exact |
| Cancel | Escape removes the working state and restores the complete entered command; inserting a probe character proves restoration at the original cursor offset | The response consumer disconnects, no boundary is appended, and the journal remains byte-exact |

The harness recorded twelve message requests in total: nine synthetic seed
turns and one held compaction request per scenario. All twelve went to the
loopback fixture; no inherited credential or commercial provider was used.
The text, PNG, raw ANSI, exact compact request, event log, final journal, and
request manifest are under `tmp/terminal-evidence/dshx-compact-states-20260911T114552Z-3372e9/`.

## Paired Claude reference UI

`scripts/claude_compaction_terminal_reference.py` exercised the current local
Claude Code v2.1.268 binary (SHA-256
`06a96d5423f83770f120859f1c58e60d7252cc4c122aa13043b7e7cd716bc76a`)
without submitting a normal conversation prompt or contacting a commercial
provider. It removed inherited credentials, used disposable home, config, and
workspace directories, selected safe mode, disabled remote control and
nonessential traffic, supplied only a dummy key, and bound the Anthropic base
URL to an HTTP fixture on `127.0.0.1`.

Each outcome used a separate disposable session. The harness created the
session with local `/copy`, directly appended three clearly marked synthetic
user/assistant pairs, resumed it, and invoked:

```text
/compact prioritize API decisions, cancellation, and exact filenames
```

The passing manifest is
`tmp/terminal-evidence/claude-compact-reference-20260911T105743Z-b21c21/result.json`. Across the three
outcomes it records three loopback token-count requests and four loopback
message requests, with zero external requests. Every message request contained
the exact focus and all three synthetic markers.

| Outcome | Claude reference observation | Durable observation |
| --- | --- | --- |
| Working | `Compacting conversation…`, a 1% progress bar, and `esc to interrupt` | Synthetic prefix unchanged while the request was held |
| Complete | `Compacted (ctrl+o to see full summary)` beneath the full command | One `compact_boundary` appended |
| Failure | `Error during compaction: API Error: 400 LOCAL_COMPACT_FAILURE` | One identical automatic request retry; no `compact_boundary` |
| Cancel | Escape removes the working display and restores the complete command to the composer | Client disconnect observed; no retry and no `compact_boundary` |

The original seeded JSONL is a byte-exact prefix of the final JSONL in all
three outcomes. Claude does append local-command bookkeeping on failure and
cancel; “no boundary” therefore does not mean its whole file is unchanged.
The complete, failure, working, and cancelled text/PNG captures, request
hashes, request summaries, and before/after JSONL files are all under
`tmp/terminal-evidence/claude-compact-reference-20260911T105743Z-b21c21/`.

## Presentation comparison

| State or surface | Claude Code v2.1.268 | dshx native held-provider PTY v4 | Result |
| --- | --- | --- | --- |
| Accepted command | Echoes the full `/compact` focus invocation | Shows the canonical `/compact` label; the current generic command-transcript policy withholds arbitrary arguments | Intentional visible safety difference, not exact presentation parity |
| In progress | Spinner, percentage bar, and Escape hint | Spinner, elapsed time, and `Esc to cancel` hint while the localhost provider is held | Same actionable state; dshx omits a synthetic percentage |
| Completed | Indented `Compacted` receipt and `ctrl+o` summary hint | Collapsed `Compacted` receipt with the same Ctrl+O affordance; expanded state preserves the full summary and collapse hint; Ctrl+O and mouse expand/collapse both pass | Source-faithful expandable outcome with keyboard and pointer evidence |
| Failure | Explicit inline error after one identical retry | Explicit normalized inline error, with no automatic retry | Same visible failure class; dshx intentionally avoids retrying a manual compaction |
| Cancel | Escape restores the full command to the composer and appends no boundary | Escape restores the full command and exact cursor offset; no boundary | Source-faithful cancellation with stronger cursor evidence |
| Original history | Synthetic prefix stays byte-exact; local-command metadata is appended | Raw journal prefix stays byte-exact and originals remain present after restart | Both retain originals append-only |

The paired Claude and dshx state captures are complete. Exact pixel parity is
not the goal: dshx deliberately withholds arbitrary command arguments from the
durable transcript, reports elapsed time instead of an invented completion
percentage, and does not retry a failed manual compaction. The working,
completed, expanded, failure, and cancellation interaction contracts are now
all evidenced through the real native terminal loop.

## Honest residual boundary

The native PTYs prove command composition, UI state handling, transport
cancellation, durable originals, continuation, and restart under controlled
providers. They do not measure how faithfully a paid model follows a focus
instruction. The Rust recording-provider tests prove the exact focused payload
and hierarchy behavior without spending provider credits; live provider
quality remains intentionally unvalidated. The Claude reference fixture proves
the current source UI and request shape against deterministic loopback
responses; it does not measure Claude's real summary quality either.
