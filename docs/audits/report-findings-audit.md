# Phase 2 `ReportFindings` native completion evidence

Date: 2026-09-11

## Disposition

`P2-T-ReportFindings` now has a real local model tool and structured UI data
path. The backend/tool contract is implemented and passes controlled tests.
The coordinated TUI consumes the typed live event and durable replay record,
but the mandatory paired live-Claude visual and interaction matrix remains
open. This row is therefore **Implemented; UI parity gate open**, not complete
Claude parity.

Nothing in this path publishes, uploads, or sends findings externally.

## Model contract and source ownership

Built-in plugin `reviewer` contributes `report_findings` in its existing
position after `work` and before `deferred-tools`. The input is a strict object
containing 1 through 128 findings. Every finding requires:

- `severity`: `critical`, `high`, `medium`, or `low`;
- the same portable cwd-relative `path` accepted by `read`, normalized in the
  durable record to the selected workspace root, plus one-based
  `line_start`/`line_end`;
- the exact 64-character lowercase revision returned by `read`; and
- bounded, trimmed, control-free `title`, `trigger`, `failure`, and `impact`
  text.

The model does not provide session, workspace, root, or cwd identity. At the
start of the mutating tool barrier, `ReviewService` pins the current
`WorkspaceTransitionService` generation. It derives the durable source from
the actual session id, workspace revision, selected root ordinal, and current
`ToolCtx` cwd relative to that root. Calls from a foreign/rebound cwd cannot
claim this source.

Each path resolves through that pinned `FileSystemService`. The service groups
aliases by resolved file, requires one revision per file, and performs a final
bounded read at the greatest referenced line with `expected_revision`
immediately before the durable append. A stale revision, missing line, escaped
path, inaccessible source, or cancellation commits and presents nothing.

Exact duplicate defects are refused even if they arrive with different local
ids. Distinct defects may share the same file and line range. Each field is at
most 8 KiB, titles are at most 256 bytes, aggregate report text is at most 256
KiB, and exact report payloads are capped at 128 findings.

## Durable and presentation ordering

The service constructs `FindingReport { id, source, findings }` and appends one
version-one `ReviewChange::FindingsReported` payload under the existing closed
`review/change` session event kind. `project_reviews` retains reports in event
order, supports exact id lookup, rejects duplicate report ids, and validates
the complete payload again during replay.

Only after `Session::append` and `Session::flush` succeed does the service emit
`UiEvent::FindingsReported { report }`. Failures and cancellation use the
ordinary tool failure plane and never emit that event. Native runtime
normalization does not feed this user-presentation event back into model input.

The coordinated TUI implementation consumes the same `FindingReport` for both
the live event and `review/change` replay. Its compact card reports count,
highest severity, workspace revision, local-only status, and bounded finding
previews. Expanded cards label location, trigger, failure, impact, and an
abbreviated revision; keyboard/mouse disclosure and accessible text are wired.

## Controlled verification

All commands ran from `/Users/naresh/Work/Personal/dshx` against the concurrent
working tree.

- `cargo test -p dshx-session --test main review_domain --no-fail-fast`: 6
  passed. New coverage proves unsafe path/revision/control refusal, exact
  duplicate rejection with same-location distinct defects allowed, and durable
  reopen/projection of source identity and findings.
- `cargo test -p dshx-agent --test main reviews --no-fail-fast`: 5 passed. New
  coverage proves current revision and maximum-line verification, normalized
  source ownership, append/flush before `UiEvent`, and zero durable/UI output
  for stale, foreign, and pre-cancelled calls. The existing delegated review
  success, mutation refusal, and retained-worktree tests remain green.
- `cargo test -p dshx-cli --test main
  default_world_reports_exact_live_inventory -- --nocapture`: 1 passed. The
  actual default composition exposes `report_findings` in the model tool
  registry.
- `cargo test -p dshx-tui finding_report -- --nocapture`: 3 passed. The
  coordinated UI owner verified session-owned live routing, durable replay,
  bounded compact/expanded rendering, mouse/keyboard disclosure, and explicit
  color-independent accessible local-only text.
- `cargo test -p dshx-session --all-targets --no-fail-fast`: 75 unit, 7
  background, and 158 integration tests passed.
- `cargo test -p dshx-agent --all-targets --no-fail-fast`: 119 unit and 316
  integration tests passed; the one explicitly ignored two-minute live timing
  test was not run.
- `cargo check -p dshx-agent --all-targets` and `cargo check -p dshx-tui --lib`:
  passed at their coordinated stable checkpoints.
- `cargo clippy -p dshx-session --all-targets -- -D warnings`: passed.
- `cargo clippy -p dshx-agent --all-targets -- -D warnings` passed after
  explicitly allowing the four warning classes already present in concurrently
  owned files (`collapsible_if`, `too_many_arguments`, `unwrap_used`, and
  `items_after_test_module`). The unmodified strict command remains blocked by
  those unrelated warnings in `agent/tool_catalog.rs`, `agent/workspace.rs`,
  `team.rs`, `work.rs`, `workspace_transition/state.rs`, `compact.rs`, and
  `subagent_config.rs`; the new review implementation and tests introduce no
  remaining warning outside that allowlist.
- `rustfmt --edition 2024 --check` over the new review domain/service/tests and
  targeted `git diff --check`: passed.

No commit, push, deployment, migration, provider credential, network call, or
external publication was performed by this work.

## Remaining gate

Controlled Rust tests establish the native contract; they do not establish
pixel/interaction parity with the current Claude reference. Retain paired
captures and keyboard/mouse evidence for applicable pending, working,
completed, failed, cancelled, grouped, long-output, and expanded states before
closing the tracker row. A live model invocation should also confirm that the
selected provider receives the exact schema and supplies a real `read`
revision without prompt-specific coercion.
