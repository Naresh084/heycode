# Phase 2 terminal navigation completion audit

Date: 2026-09-11 (Australia/Melbourne)

## Disposition

The bounded terminal implementation owned by this work is complete and passes
controlled QA: the bottom agent navigator, agent focus/return behavior,
responsive agent layouts, semantic dark/light/color-disabled rendering,
`/focus`, `/color`, accepted slash-command transcript rows, and the typed
findings-report card.

This does **not** close Phase 2 as a whole. In particular, the tracker contract
for `P2-UI02` still asks for a real-provider dshx journey, and `P2-UI03` still
asks for paired Claude/dshx evidence for every applicable tool, command, working,
error, permission, cancellation, and expansion state. The controlled native and
fake-provider journeys below do not satisfy those broader gates.

No tracker row or shared Phase 2 planning document was edited by this work. No
commit, push, deployment, migration, credential change, or findings publication
was performed. The dshx implementation journeys used only localhost/fake
provider paths; the comparison source is a separately recorded authorized
Claude session.

## Implemented terminal behavior

### `P2-AG02` — bottom agent navigator

- Agent rows are sourced from the admitted session-owned agent inventory rather
  than from the mixed job/tool/team task count.
- The persistent footer exposes direct agent names and states plus an explicit
  conversation selector. Work, Jobs/background activity, Teams, and Workflows
  retain separate identities and controls.
- The inline spawn tree, footer rows, background browser, and inspector use
  stable task keys and consistent current-owner semantics.
- Keyboard and mouse hit regions open an agent, return to the parent, inspect a
  different agent without stealing ownership, and foreground a selected agent.

### `P2-AG03` — focus, return, cancellation, and state isolation

- Parent and child composer drafts are stored independently and restored when
  switching owners. The same owner boundary applies to paste state and
  transcript scroll/follow behavior.
- Opening an inspector does not change the active conversation owner. Returning
  from a child restores the parent's draft and visible transcript.
- Child lifecycle rows retain running/settled/cancelled distinctions; the
  inspector exposes prompt, progress, elapsed time, model, and relevant actions.
- A composer submission returns the transcript to follow mode.

### `P2-AG04` — scale and responsive layout

- Regression coverage exercises 1, 5, and 20 agents with long Unicode labels at
  80 × 24, 100 × 45, and 160 × 45 cells.
- The final PTY journey uses five live native child owners at 100 × 45 and then
  resizes to 52 × 30. The active row, child draft, status, tool result, and
  selector remain reachable without horizontal layout failure.

### `P2-UI01` — semantic rendering in this scope

- The terminal theme supplies semantic text, dim, accent, success, warning,
  error, and code roles to the navigation, inspector, transcript, and Markdown
  renderer.
- Dark and light terminal journeys pass. A separate color-disabled journey
  verifies that no foreground/background SGR is emitted and that labels and
  symbols still communicate state.
- Session colors are resolved at the terminal's supported color tier. Truecolor
  uses a contrast-aware dark-surface or light-surface palette; basic, ANSI-256,
  and no-color terminals degrade without inventing unsupported color output.
- The final light cyan session accent is `#0e7490` against the light surface;
  the dark equivalent remains `#22d3ee`.

### `/focus`, `/color`, and accepted command rows

- `/focus` is a TUI-only queued control. It keeps the current user intent,
  condensed tool outcomes, final answer, durable title, and an explicit count of
  hidden transcript items. The preference persists across restart.
- `/color [color]` accepts a named color, random selection with no argument, and
  `default`. It changes only the current terminal session identity and resets on
  restart; it does not replace the global theme.
- Accepted slash commands render as `Item::Command`, not `Item::User`, so they
  do not become model input. Only a small reviewed allowlist may expose bounded
  arguments; secret/path-bearing commands render only their command name.
- The session title is replayed from its durable session event and appears in the
  composer divider. Focus and color commands do not create durable user-message
  events.

### Findings-report presentation add-on

`UiEvent::FindingsReported` and durable `ReviewChange::FindingsReported` replay
produce the same typed `FindingReport` card. The compact view includes the
finding count, highest severity, workspace revision, local-only status, and a
bounded preview. The expanded view includes severity, location, title, trigger,
failure, impact, and abbreviated revision for up to 12 findings while retaining
the complete report in session state.

Mouse and keyboard disclosure, focus, transcript height/fingerprint accounting,
and the screen-reader projection are covered. The card says that it is local and
not externally published; no report is converted into a model message. A
one-column height-bound regression caught and fixed focused disclosure text that
had not been included in the viewport budget.

### Host-owned detach shortcut

Raw `Ctrl+]` is reserved for the whole-session host detach path. User keymap
resolution now rejects any attempt to bind it, before settings persistence, with
the explicit error: “ctrl+] is reserved by the session host for detach”.

## Final controlled evidence

The final binary is `tmp/cli-snapshots/3367999eec7f6af2/dshx-20260911T090955Z-fef429`, SHA-256
`3367999eec7f6af2bed6b5dbe610b6be59124aea6281183bc1e2c2d6eaa3fb55`.

### Rust verification

- `cargo check -p dshx-tui --lib` — passed.
- `cargo test -p dshx-tui --no-fail-fast --quiet` — 86 unit tests passed,
  2 existing voice tests ignored, 331 integration tests passed, and 7 standalone
  memory-command tests passed: 424 passed, 0 failed.
- `cargo test -p dshx-ui --no-fail-fast --quiet` — 61 passed, 0 failed.
- Focused tests passed for `/focus`/`/color`, findings live/replay/rendering and
  accessibility, focus preference persistence, prompt color tier resolution,
  accepted command labels, composer follow mode, findings-card height bounds,
  and the host detach keymap reservation.
- `cargo fmt --all -- --check` — passed.
- `cargo clippy -p dshx-tui --all-targets --no-deps -- -D warnings` — passed.
  The owned renderer warning (identical selected/unselected foreground branches)
  and the new test warning are fixed; the concurrently owned command-test
  boolean comparison found by the first run was also corrected before this
  final strict run.
- Targeted `git diff --check` over the owned renderer, transcript, keymap, and
  PTY scripts — passed.

### Final dshx PTY journeys

All paths below contain the PNG/text captures and a machine-readable
`result.json` with the final binary hash.

| Journey | Runtime and transport | Result |
|---|---|---|
| `tmp/terminal-evidence/navigation-dark-20260911T091055Z-b1e0f6/` | Real dshx CLI/TUI and native agent runtime; deterministic localhost SSE fixture | Passed 8 screens, 12 fixture requests |
| `tmp/terminal-evidence/navigation-light-20260911T091055Z-bd8bda/` | Same controlled native path, light theme | Passed 8 screens, 12 fixture requests |
| `tmp/terminal-evidence/navigation-no-color-20260911T091022Z-a404bc/` | Same controlled native path, color disabled | Passed 8 screens, 12 fixture requests; no color SGR |
| `tmp/terminal-evidence/dshx-focus-color-dark-20260911T091022Z-3f77fa/` | Real dshx CLI/TUI; built-in fake inference; disposable home/workspace | Passed 9 screens |
| `tmp/terminal-evidence/dshx-focus-color-light-20260911T091022Z-693221/` | Same controlled path, light theme | Passed 9 screens |

The navigation journeys verify five concurrent admitted children, keyboard
entry, a retained child read result, mouse owner switching, isolated
parent/child drafts, narrow resize, background browser, and inspector ownership.
The focus/color journeys verify normal/focused/restored transcripts,
random/cyan/default colors, durable title replay, focus persistence, color reset,
and exactly one durable user message containing only the deliberate fake turn.

### Current Claude comparison source

- `tmp/terminal-evidence/claude-terminal-20260911T081009Z-7bed76/` is a passed Claude Code 2.1.268
  disposable run containing the main switcher, background list/results,
  inspector, and foreground draft.
- `tmp/terminal-evidence/claude-focus-color-20260911T075049Z-03165a/` is a passed Claude Code 2.1.268
  disposable run containing normal/focus/restored and
  no-argument/cyan/default color states.
- `tmp/terminal-evidence/comparisons-20260911T085846Z-f3f7ad/` places the source and dshx captures in the
  same comparison inputs. Source and implementation use 100 × 45 terminal cells
  and 1032 × 977 PNGs; the responsive dshx capture uses 52 × 30 cells and
  552 × 662 pixels. No CSS or browser density conversion applies.

The scoped visual result is recorded in the project-root `design-qa.md`: no open
P0, P1, or P2 finding remains for navigation/focus/color. The dshx banner, five
agents rather than two, its persistent multi-agent footer, and retained accepted
command rows are documented intentional differences.

## Final `/config` and `/copy` closure addendum

The four-child settings shell and Copy chooser were subsequently closed against
fresh source and immutable-binary evidence. These command closures do not change
the broader `P2-UI02`/`P2-UI03` boundaries above.

`P2-C-config` is complete. Claude Code 2.1.268 source behavior is retained in
`tmp/terminal-evidence/config-shell-claude-20260911T110959Z-a0f6f9/` with 19 captures. The final dshx run
is `tmp/terminal-evidence/settings-shell-dshx-four-tabs-20260911T115442Z-70ea0c/`, produced by
`scripts/settings_terminal_check.py` against immutable binary SHA-256
`25526dce041310f5ccf263a406ef0717afeed43616a343ecd6dc8a278fce5010`.
Its 14 full-screen PTY captures verify the direct `/status`, `/config`, `/usage`,
and `/stats` children, the canonical `/settings` Config entry, bidirectional tab
wrap, Config search/row/tab focus, and actual pointer selection of tabs, search,
and a row. The journal contains no durable user, assistant, or request event.
The exact `copy` filter comparison is retained as
`tmp/terminal-evidence/settings-shell-paired-config-exact-20260911T115317Z-002eac.png`.

The Stats child in that v6 run was explicitly unavailable because that build
did not yet have a cross-session aggregate owner. The later Stats-owner follow-up
below supersedes that interim state without reopening `P2-C-config`.

`P2-C-copy` is also complete. The 13-state Claude source contract is in
`tmp/terminal-evidence/command-reference-claude-copy-20260911T101531Z-f0be02/`, the source preference,
restart, and Config-revert branch is in
`tmp/terminal-evidence/copy-claude-persistent-20260911T110959Z-9a29b0/`, and the 19-state dshx picker run
is in `tmp/terminal-evidence/copy-dshx-native-polish-20260911T105114Z-d5a99e/`. The final v6 table
gate is `tmp/terminal-evidence/copy-table-dshx-20260911T114650Z-b3e6f7/`, reproduced by
`scripts/table_copy_terminal_check.py`. Its OSC 52 payload, private `0700`/`0600`
recovery file, and Claude normalized fixture are byte-identical: 150 bytes,
SHA-256 `5eb06e828f44403c35f2f7ccab3e6d7344b4f139cd47b9674817ba05b8f3fb45`.
Fenced regions remain unchanged and `/copy` never enters model input. The
paired picker comparison is `tmp/terminal-evidence/copy-table-paired-20260911T114713Z-2529a7.png`.

## Tracker-boundary assessment

| Tracker row | Result from this work | Boundary that remains |
|---|---|---|
| `P2-AG02` | Passed controlled implementation and terminal QA | Live commercial-provider evidence is outside this run |
| `P2-AG03` | Passed controlled keyboard/mouse/draft/inspector behavior | Broader provider recovery/cancellation certification remains coordinator-owned |
| `P2-AG04` | Passed 1/5/20 regression matrix and final 5-agent responsive PTY | Other terminals/platforms are not certified |
| `P2-UI01` | Passed for the owned navigation/focus/color surface in dark/light/no-color | This is not every Phase 2 screen |
| `P2-UI02` | Controlled native equivalent passed | Strict real-provider dshx journey remains open |
| `P2-UI03` | Current Claude comparisons passed for the owned subset | Every other applicable tool/command/state remains open |
| `P2-QA01` | This scoped matrix and comparison artifact are complete | Full-workspace and all-owner reconciliation remains coordinator-owned |
| `P2-C-config` | Complete: source-mapped four-child shell, direct commands, keyboard, mouse, help/provenance/error and canonical `/settings` evidence | Cross-session Stats content is tracked separately and does not reopen Config |
| `P2-C-copy` | Complete: source picker, errors, copy/write/cancel, persistence/restart/revert, exact table normalization and private recovery | Shared transcript table rendering is a separate `P2-UI01` concern |
| `P2-C-usage` | Complete for the adopted provider-neutral contract: current-session Usage plus bounded durable local Stats, with empty and non-empty paired terminal evidence | Claude subscription/account limits, billing truth, and live-provider reconciliation are not fabricated or claimed |

## Durable Stats-owner follow-up

`P2-C-usage` now has a real local cross-session statistics owner rather than an
unavailable placeholder. `SessionQueryService::stats` performs one bounded
JSONL-store scan and projects only each physical session suffix, so inherited
fork prefixes are not counted once per descendant. It includes active and
archived sessions, excludes recoverable trash, reports unreadable logs as
partial coverage, and keeps missing token, route, and cache-write evidence
explicit. The typed snapshot owns message totals, UTC activity days and
streaks, UTC peak hour, recorded message spans, provider-reported token lower
bounds, model attribution, and detailed cache counters.

The settings snapshot runs that disk projection on a blocking worker and keeps
four states distinct: Ready, Empty, Unavailable for replacement providers that
do not own statistics, and Failed for an unsuccessful read. The rendered view
labels its active+archived scope and UTC convention and bounds recent-day and
model rows.

Verification completed before the requested pause:

- `cargo test -p dshx-session --all-targets`: 245 passed, 0 failed.
- `cargo test -p dshx-status --all-targets`: 33 passed, 0 failed.
- `cargo clippy -p dshx-session -p dshx-status --all-targets -- -D warnings`:
  passed.
- `tmp/terminal-evidence/settings-shell-dshx-stats-owner-empty-20260911T121925Z-fe653e/result.json`:
  real full-screen PTY, immutable binary SHA-256
  `41f94417ef1ba5ffa745d67d97a12c99164ad68ae38c1aae66e48d6d2904451d`,
  14 keyboard/mouse captures, zero durable model-input events, and the
  authoritative Stats Empty state.
- `tmp/terminal-evidence/settings-shell-paired-stats-empty-20260911T122001Z-1bae74.png`: visually inspected
  side-by-side Claude Code 2.1.268 and dshx empty-state comparison.
- `tmp/terminal-evidence/settings-shell-paired-stats-nonempty-20260911T192232Z-cb854f/result.json`: passed
  paired non-empty full-screen PTY run at the same 110 × 46 terminal viewport.
  The source side is Claude Code 2.1.268 binary SHA-256
  `06a96d5423f83770f120859f1c58e60d7252cc4c122aa13043b7e7cd716bc76a`
  over a synthetic version-five Stats cache plus one discoverable synthetic
  transcript in a disposable `CLAUDE_CONFIG_DIR`. The implementation side is
  immutable dshx v11 SHA-256
  `14fc1b07ac6415a36d863dc4a436b0e2c8a9b26e1880ab98db6130d14f6e7f3d`
  over two CLI-created durable local sessions, one archived, enriched with
  deterministic reported token and cache facts. Neither side sent a
  conversation prompt to an external model or contacted an external provider.
- `tmp/terminal-evidence/settings-shell-paired-stats-nonempty-20260911T192232Z-cb854f/paired-stats-nonempty.png`,
  the source screenshot, and both dshx top/scrolled screenshots were visually
  inspected. Claude shows its heatmap, model, session, streak, token, and cache
  summary. dshx shows explicit scan scope/coverage, active and archived counts,
  message/activity/span facts, reported token/cache accounting, favorite-model
  attribution, and the bounded recent-day row. The existing shell pointer test
  covers Stats tab selection; this non-empty run additionally exercises content
  focus and End-scroll. Contrast and keyboard instructions are visible, but no
  full assistive-technology certification is claimed.
- Focused current-source regressions pass: two Stats tests in `dshx-session`,
  two Stats tests in `dshx-status`, and strict all-target Clippy for both crates
  with `--no-deps -- -D warnings`. Logs are
  `tmp/stats-session-focused-20260911T192345Z-5857ab.log`,
  `tmp/stats-status-focused-20260911T192503Z-567884.log`, and
  `tmp/stats-clippy-20260911T192556Z-8c9cb5.log`.

The storage regression includes an archived parent and active fork, verifies
that the parent messages/tokens/cache counters appear once rather than twice,
then moves the child to recoverable trash and verifies it leaves the aggregate.
Separate tests cover unreadable-log lower bounds, UTC day/streak grouping,
reported versus unreported usage, unattributed responses, unknown cache writes,
and Ready/Empty/Unavailable/Failed shell classification. Ready now has retained
non-empty source/native terminal evidence, Empty has retained paired evidence,
and the replacement-provider Unavailable plus store-read Failed states remain
covered by typed integration tests because they have no source-equivalent
Claude Stats states. Read-only Stats has no applicable asynchronous working or
cancellation lifecycle.

`P2-C-usage` is therefore complete for the adopted local provider-neutral
contract. This verdict does not reinterpret Claude's account-level Usage tab as
local durable data: dshx has no authoritative subscription quota or billing
owner, so provider/account limit presentation remains outside this command
contract until a provider-specific capability exists.

## Explicit remaining gates

1. Run dshx against an authorized real commercial provider through the five-file
   journey; separate provider correctness from terminal rendering.
2. Complete the paired Claude/dshx matrix for every applicable tool, command,
   pending/working/queued/error/permission/cancellation/long-output/expanded
   state, including keyboard, mouse, owner, and draft checks.
3. Invoke `report_findings` with a live tool-capable model and verify that it uses
   an exact revision returned by `read`; the current backend and TUI proof is
   controlled only.
4. Reconcile the complete workspace test/clippy result across all concurrently
   owned Phase 2 modules before declaring the overall phase ready.
