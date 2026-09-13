# Phase 2 remaining tool gates and shared live matrix

Date: 2026-09-12 (Australia/Melbourne)

## Scope and verdict

This audit covers the 37 `P2-T-*` rows whose current status is `validating` in
`docs/terminal-compatibility-tracker.json`, plus the coordinator-requested filesystem rows
`P2-FS02` through `P2-FS06` and the `P2-TL01` and `P2-WS01` foundation rows.
`P2-T-ToolSearch` is `deferred` and is excluded. Code mode and `run_code` are
also excluded; the Workflow row is assessed only through the independently
registered `workflow` surface.

One row is ready to be marked complete from the inspected evidence:
`P2-T-TodoWrite`. Its acceptance is removal from new requests, not a new tool
lifecycle. The saved configured dshx request inventory advertises all four
structured Task tools and omits `todo_write`; the production registry asserts
the same; historical successful TodoWrite events migrate read-only into Work;
and an immutable production binary visibly rejects a deliberately injected,
unadvertised `todo_write` call as unknown. There are no applicable new-tool
pending/working/completed states to fabricate. Structured Work presentation
remains owned by the six Task-family rows.

No other validating row is ready to be marked complete. Most native contracts
have strong controlled coverage, and several have real local CLI/PTY or
third-party-process canaries. The remaining common blocker is the
explicit user gate in `docs/terminal-compatibility-requirements.md`: a current Claude and dshx
comparison at the same terminal size and equivalent state, with real request
and execution evidence plus retained keyboard/mouse assertions. A localhost
model fixture is useful production-path evidence but is not a configured-model
comparison. A renderer/unit test is not live UI evidence.

The retained readiness artifact at
`tmp/terminal-evidence/live-model-readiness-20260911T105718Z-eb556a` proves response access through real
Claude Code 2.1.268 (`claude-opus-5`, first-party Max) and the saved dshx
OpenRouter route (`meta/muse-spark-1.3`). Both answered exactly `READY`. The
dshx receipt records one request, 13,233 prompt and 18 completion tokens, with
zero tool events. Claude records 2 input, 3,310 cache-creation input and 4
output tokens, no server tool use, and a USD 0.03321 list-price estimate (not a
separately proven charge). Cleanup is recorded. This removes an access
precondition but closes no tool row: the readiness turn deliberately invoked
no tool and did not capture the substantive request's full schema inventory.
It is a product-effect comparison between different model/product
configurations, not a same-model harness comparison. Do not repeat this paid
readiness prompt; capture schemas on the first substantive request instead.

The later shared core pilot at `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf` now supplies
that first substantive comparison for Glob, Grep, Read, Edit, and foreground
Bash. Each product received one user prompt and made six successful calls:
Glob, Grep, Read twice, Edit, and Bash. Both read the intended target before
editing, changed only `src/config.txt` from `mode=slow` to `mode=fast`, ran
exactly `python3 checks/check.py`, returned `CORE_CHECK_OK`, and left the other
three fixture files byte-identical. dshx also passed all five same-request
schema assertions and supplied the Read revision in Edit. Its six request
headers record 88,330 prompt and 1,352 completion tokens at defaulted `max`
effort. Claude records 10 direct input, 8,090 cache-creation input, 30,050
cache-read input and 919 output tokens at `high`; its actual call
names/arguments/results are retained, but exact outbound tool definitions are
unavailable through the inspected credential-safe normal CLI surfaces. These
different effort/cache routes are configured facts, not a latency or cost
benchmark. The pilot also records dshx `web_search` routed provider-native to
OpenRouter and client `web_fetch`; neither web surface was invoked.

The subsequent B1 affirmative pilot at
`tmp/terminal-evidence/plan-review-live-20260911T120703Z-c85f67` adds one configured-model run per product
for `AskUserQuestion`, `EnterPlanMode`, `Read`, `ExitPlanMode`, `Edit`, and a
verification `Read`. Each user chose Beta, reviewed the complete plan, manually
accepted the plan, inspected the exact `BEFORE` → `AFTER` diff, approved one
edit, and finished with identical six-byte `AFTER\n` files. Native retained six
successful calls across seven request headers; Claude retained seven calls
across eight model response ids. The native Edit carried the exact revision
from the first Read. This closes the affirmative B1 functional path only. It
does not close Alpha/free text, rejection/cancellation/feedback, pending reopen,
restart or B2/B3. Retained paired captures also expose unresolved presentation
differences: extra native entry/Read approvals, misleading broader-policy help
under the single-call Accept selection, different plan defaults/navigation,
clipped completed-tool JSON, and question/wrapping differences. The helper and
Plan result summaries have since been corrected in shared source; the frozen
v7 captures remain honest evidence of the run, while the fixes await a rebuilt
no-cost capture.

This file does not edit tracker status, submit a commercial prompt, or claim
remote/hosted behavior from a local equivalent. The later Grep reconciliation
below does update the local contract, but it does not convert controlled source
and test evidence into configured-model or presentation closure.

## Grep source-contract reconciliation

The installed Claude Code 2.1.269 executable's own Grep help identifies
`content`, `files_with_matches`, and `count` output modes (files are the
default), `glob`, `-A`/`-B`/`-C`/`context`, `-n`, `-i`, `only_matching`, `type`,
`head_limit`, `offset`, and `multiline`. The retained core pilot independently
contains an actual Claude Grep call with
`{"pattern":"mode=slow","output_mode":"content","-n":true}` and a
`path:line:text` result. This is stronger source evidence than the previous
three-field comparison, but it is still not an exact credential-safe outbound
tool definition; that gap remains explicit.

Current dshx now implements the bounded, supportable subset instead of silently
retaining the earlier divergence: the three output modes, default
`files_with_matches`, path-aware `glob` plus the legacy `include` alias,
case-insensitive matching, optional line numbers, bounded before/after/context,
and bounded `head_limit`/`offset`. The exec provider returns typed content
context or typed per-file counts, keeps stable global ordering and existing
64-MiB/file, 256-MiB aggregate, 24-KiB retained-output caps, and the service
rejects mode-confused, impossible, unsafe, or out-of-order provider results.
Unsupported `type`, `multiline`, and `only_matching` are not advertised and
fail explicitly if passed directly; no exact-parity claim is made for them.

Focused tests cover default files, content, count, case-insensitivity,
path-aware globs, context merging and group separators, line-number omission,
offset/head pagination, conflicts, unsupported arguments, provider-output
validation, cancellation, binary/oversized input and partial notices. The
configured-model edge run remains unexecuted. Its single bounded, shared-prompt
proposal is retained at
`tmp/terminal-evidence/filesystem-edge-acceptance-plan-20260911T200231Z-9699d9.md` for coordinator review;
preparing it caused no inference or fixture mutation.

## Closure reclassification

The coordinator's staged matrix is retained as written; B/C/D are scenario
families, not evidence levels:

- **Stage A / S1-S2:** files/local output, then shell/processes;
- **Stage B / S3-S4:** human decisions and plan review, then structured Work
  and agents;
- **Stage C / S5-S6:** schedules, then MCP/LSP and skills; and
- **Stage D / S7-S8:** worktrees/review/workflows, then native web.

Availability, paired presentation, lifecycle, interaction/layout and
durability remain the A/P/L/I/R evidence dimensions defined below. A
`configured success baseline` proves only the named affirmative path. It does
not close omitted branches or Claude's credential-safe outbound-schema gap.

| Row | Matrix stage and highest retained evidence | Exact open gate | Decision |
| --- | --- | --- | --- |
| `P2-T-Artifact` | A/S1; controlled contract complete | A plus P/L/I/R: configured invocation/Claude availability and register/list/preview/stale/remove/missing/replay cards. | Keep `validating`. |
| `P2-T-Edit` | A/S1; configured core success and B1 exact-diff success | A: Claude outbound definition unavailable. P/L/I/R: denial/match error/stale/no-op/cancel/group/long/replay and final modes. | Keep `validating`. |
| `P2-T-Glob` | A/S1; configured core success | A: Claude definition unavailable. P/L/I/R: empty/cap/partial/denied/unreadable/cancel/group/expand/replay and modes. | Keep `validating`. |
| `P2-T-Grep` | A/S1; configured core success plus reconciled bounded local modes | A: Claude's exact outbound definition is unavailable; `type`, `multiline`, and `only_matching` remain explicitly unsupported rather than inferred. P/L/I/R: configured empty/collision/long/binary/partial/denied/error/cancel/replay and modes. | Keep `validating`. |
| `P2-T-NotebookEdit` | A/S1; controlled contract complete | A/P/L/I/R: configured paired replace/insert/delete/stale/invalid/deny/cancel/group/replay and modes. | Keep `validating`. |
| `P2-T-Read` | A/S1; configured core success and B1 reads | A: Claude definition unavailable. P/L/I/R: paging/range/batch/empty/missing/binary/long/stale/denied/cancel/replay/modes and rich-input mapping. | Keep `validating`. |
| `P2-T-SendUserFile` | A/S1; real local ATT01 PTY | A/P/L/I/R: configured dshx selection, current Claude availability/schema, full one/many/refusal/cancel/save/replay/mode pair. | Keep `validating`; remote delivery is out of scope. |
| `P2-T-Write` | A/S1; controlled contract plus no-cost local cards | A/P/L/I/R: configured create/no-op/replace/stale/refusal/denial/cancel pair, exact preview/long/group/expand/replay and modes. | Keep `validating`. |
| `P2-T-AskUserQuestion` | B1/S3; configured affirmative Beta path complete | P/L/I/R: Alpha/free-text/cancel/pending reopen/restart/duplicate refusal, focus and display modes; captured alternatives/wrapping differ. | Keep `validating`. |
| `P2-T-EnterPlanMode` | B1/S3; configured affirmative entry complete | P/L/I/R: reentry/same-batch refusal/interruption/restart/modes; extra native approval remains. Current source replaces clipped completed JSON with a typed summary; rebuilt capture pending. | Keep `validating`. |
| `P2-T-ExitPlanMode` | B1/S3; configured Default-policy acceptance complete | P/L/I/R: accept-with-edits/feedback/reject/cancel/apply failure/restart/modes; defaults/navigation differ. | Keep `validating`. |
| `P2-T-TaskCreate` | B2/S4; controlled contract complete | A/P/L/I/R: configured stable-id create pair, invalid owner/dependency, concurrency/grouping/restart and modes. | Keep `validating`. |
| `P2-T-TaskGet` | B2/S4; controlled contract complete | A/P/L/I/R: configured live/revised/tombstone/unknown/foreign pair, detail/long/error/replay and modes. | Keep `validating`. |
| `P2-T-TaskList` | B2/S4; controlled contract and real local console | A/P/L/I/R: configured empty/populated/filter/page pair, multiple/blocked/ready/deleted/replay/domain isolation and modes. | Keep `validating`. |
| `P2-T-TaskUpdate` | B2/S4; controlled contract complete | A/P/L/I/R: configured concurrent update/stale CAS/cycle rejection pair, field/status/block/delete/tombstone/replay and modes. | Keep `validating`. |
| `P2-T-TodoWrite` | B2/S4 replacement; configured request absence, negative production-binary call and legacy migration | No new-tool lifecycle is applicable. Structured Work UI stays on the Task rows. | **Ready `complete`.** |
| `P2-T-ListAgents` | B3/S4; controlled contract and real five-agent local matrix | A/P/L/I/R: configured exact model tool/Claude receipt; 0/1/5/20 states, paging/navigation/follow-up/replay and modes. | Keep `validating`. |
| `P2-T-SendMessage` | B3/S4; controlled contract complete | A/P/L/I/R: configured busy/idle/stopped/unknown/foreign pair; delivery/error/unread/return/replay and modes. | Keep `validating`. |
| `P2-T-TaskOutput` | B3/S4 and A2/S2 cross-domain; controlled retained-output core | A/P/L/I/R: configured running/settled output pair, two-page/offset/truncation/missing/foreign/replay and modes. | Keep `validating`. |
| `P2-T-TaskStop` | B3/S4 and A2/S2 cross-domain; typed controlled stop core | A/P/L/I/R: configured agent/process stop pair; stopping/settled/already/missing/foreign/teardown/replay and modes. | Keep `validating`. |
| `P2-T-Bash` | A2/S2; configured foreground success plus no-cost deny/long/expand | A: Claude definition unavailable. P/L/I/R: no-output/non-zero/timeout/interruption/background/paged output/replay and all modes. | Keep `validating`. |
| `P2-T-Monitor` | A2/S2; controlled core | Define comparable Claude source families, then A/P/L/I/R for configured stream/no-match/dedup/end/timeout/cancel/foreign/replay and modes. | Keep `validating`. |
| `P2-T-CronCreate` | C1/S5; controlled contract, real local PTY and one due fire | A/P/L/I/R: configured model/current Claude; valid/invalid/cap/expiry/approval/error/restart pair. | Keep `validating`. |
| `P2-T-CronDelete` | C1/S5; controlled contract and real local PTY | A/P/L/I/R: configured model/current Claude; missing/foreign/fired/admitted-race/restart pair. | Keep `validating`. |
| `P2-T-CronList` | C1/S5; controlled contract and real local PTY | A/P/L/I/R: configured model/current Claude; empty/multiple/expired/deleted/paged/restart pair. | Keep `validating`. |
| `P2-T-ScheduleWakeup` | C1/S5; controlled contract, real local PTY and due fire | A/P/L/I/R: configured model/current Claude; awaiting/reschedule/stop/expired/invalid/race/non-restoration pair. | Keep `validating`. |
| `P2-T-ListMcpResourcesTool` | C2/S6; controlled contract and real XcodeBuildMCP canary | A/P/L/I/R: configured same-server pair; zero/connecting/paged/empty/failure/disappear/stale/replay and modes. | Keep `validating`. |
| `P2-T-ReadMcpResourceTool` | C2/S6; controlled contract and real XcodeBuildMCP canary | A/P/L/I/R: configured same-server pair; text/binary/long/stale URI/failure/auth/cancel/disappear/replay and modes. | Keep `validating`. |
| `P2-T-WaitForMcpServers` | C2/S6; controlled contract and real ready canary | A/P/L/I/R: configured slow-server/Claude equivalent; zero/ready/timeout/failure/auth/disappear/cancel/replay and modes. | Keep `validating`. |
| `P2-T-LSP` | C2/S6; exact fixture plus Apple clangd canary | A/P/L/I/R: configured call/Claude availability, operation/error/empty/long/cancel/replay/modes; add server canaries or narrow claim. | Keep `validating`. |
| `P2-T-Skill` | C2/S6; controlled contract complete | A/P/L/I/R: configured same-fixture load/Claude equivalent; unknown/malformed/oversized/removed/restart/attribution and modes. | Keep `validating`. |
| `P2-T-EnterWorktree` | D1/S7; controlled model loop complete | A/P/L/I/R: saved configured model/current Claude; approval/dirty/busy/refusal/recovery/restart pair. | Keep `validating`. |
| `P2-T-ExitWorktree` | D1/S7; controlled contract complete | A/P/L/I/R: saved configured model/current Claude; success/not-entered/busy/recovery/restart pair and explicit retained-worktree receipt. | Keep `validating`. |
| `P2-T-ReportFindings` | D1/S7; controlled contract plus real dshx/fixture-Claude cards | A/P/L/I/R: unscripted configured pair; multi-file/severity/empty/invalid/deny/cancel/long/group/replay and dense-approval fix. | Keep `validating`. |
| `P2-T-Workflow` | D1/S7; controlled contract and real localhost PTY | A/P/L/I/R: configured dshx/truthful Claude equivalent; progress/success/failure/cancel/checkpoint/resume/group/long/replay and modes. | Keep `validating`. |
| `P2-T-WebFetch` | D2/S8; controlled extraction/security core | A/P/L/I/R: configured same-public-source pair; normal/empty/redirect/private block/error/cancel/long/citations/replay and modes. | Keep `validating`. |
| `P2-T-WebSearch` | D2/S8; routing policy only | A/P/L/I: actual saved-route search/citations/usage plus OpenAI/Codex, Anthropic, OpenRouter and xAI gating; success/zero/error/cancel/long/citation expansion/modes. | Keep `validating`. |

Foundation disposition is unchanged: `P2-FS02` still needs the error-class UI
edge inventory, `P2-TL01` still needs prerequisite loss/restoration and
cross-row reconciliation plus the closest honest Claude surface, and
`P2-WS01` shares WebSearch's provider/citation/usage matrix. None is ready for
`complete`.

## B1 presentation triage after current-source fixes

The frozen B1 run is behaviorally correct. Current shared source has already
fixed its two clearest misleading compact surfaces: selection zero now says
that it approves only this request while preserving policy, and successful
Enter/ExitPlanMode results now use typed summaries instead of clipped raw JSON.
Do not repeat the configured-model prompt merely to recapture those fixes.

The remaining targeted presentation work is:

1. **Add real pointer ownership to both decision surfaces.** The runtime
   question handler consumes keys and paste but ignores mouse clicks. The plan
   review handles mouse-wheel scrolling only; clicking one of its three visible
   choices does nothing. Retain per-frame hit rectangles, make one click select
   without accidental submission, provide an explicit mouse confirmation path,
   and cover clicks on wrapped choice/description rows plus clicks outside the
   panel. This is a direct mandatory-interaction gap.
2. **Remove hard-coded plan-review color.** `plan_review.rs` uses
   `Color::Cyan` and default border/text styles instead of the active terminal
   `Styles`. Pass the semantic palette into the renderer and verify selected,
   unselected, border and feedback text at dark, light and `NO_COLOR`, including
   a narrow viewport.
3. **Wrap user turns at word/grapheme boundaries.** `wrap_user_message` is
   character-count wrapping, which explains the observed B1 mid-word splits.
   Reuse the existing word/grapheme wrapper with a bounded fallback for one
   unbreakable token; preserve selection background and exact copied text.
4. **Humanize the remaining plan tool headings.** The result body is now typed,
   but `tool_display_name` still falls through to `enter_plan_mode` and
   `exit_plan_mode`. Map those headings to `EnterPlanMode` and `ExitPlanMode`
   without changing their canonical request names or journal data.
5. **Keep long feedback reachable.** The plan footer is fixed at nine rows
   while feedback is bounded at 16 KiB. Give feedback its own bounded visible
   editor/scroll state or calculate a safe dynamic footer, then test long
   Unicode paste, backspace, resize and complete document reachability.

Two observed differences are not automatic defects. Claude used an explicit
tool allowlist while dshx used Default ask-each-time policy, so the extra Read
approvals are not a like-for-like policy comparison. Preserve fail-closed
approval until an equivalent configuration is compared. Likewise, dshx's
initial **stay in Plan mode** selection is the safer default; do not flip it to
Claude's auto-mode default solely for visual parity. The native `Other` answer
is truthful free-text support; Claude's separate chat-about-this branch should
not be copied unless that distinct behavior is adopted.

## B2 Work presentation correction in current source

The current native Work console now keeps durable Work records separate from
running agent and process semantics. Opening a Work row is a read-only preview:
it preserves the parent composer, never creates an `@agent` composer, and does
not expose steering, foreground, interrupt, stream, execution elapsed, tool
progress or prompt affordances. The visual and accessible views use the Work
labels `pending`, `in progress` and `blocked`; generic running/waiting execution
counts exclude Work. The detail view retains the stable Work ID, revision,
owner, exact dependency IDs, description, metadata and result, and keyboard
and real mouse opening return without changing the record.

`cargo test -p dshx-tui --test main task_console -- --nocapture` passes all 30
task-console tests, including the expanded narrow/wide Work regression at
80x24, 100x45 and 160x45. `cargo clippy -p dshx-tui --test main --no-deps --
-D warnings` also passes. These are controlled local implementation checks;
they do **not** close any of `P2-T-TaskCreate`, `P2-T-TaskGet`,
`P2-T-TaskList` or `P2-T-TaskUpdate`. Their configured-model/current-Claude,
negative-state, concurrency, restart/replay and remaining interaction/display
gates below stay explicit.

The focused loopback-only production-path check
`scripts/task_console_pty.py --work-only` also passes against immutable
`tmp/cli-snapshots/b1ea2c1501db8cdb/dshx` (SHA-256
`b1ea2c1501db8cdbadfb1775246c90b7f975b659e14de7e9142198dc64c2d0f8`).
Its single retained `work-detail` screen and receipt are under
`tmp/terminal-evidence/work-presentation-20260911T193309Z-07764b`. The run made two localhost fixture
requests and no commercial-provider request. Visual inspection confirms the
pending Work title and stable ID, state, revision, owner, dependencies,
description and metadata; the preview has only close/back controls and no
agent/process elapsed, prompt, foreground, interrupt or terminal-input copy.
This is real native TUI evidence for the correction, not configured-model or
Claude-pair evidence, and it closes no Task-family row.

## New no-cost production-binary UI evidence

Three bounded loopback-only journeys were rerun against immutable
`tmp/cli-snapshots/6b23742beb414b34/dshx` (SHA-256
`6b23742beb414b34527412e652ba1147adbd05e5f9558442964a057b996d1e60`).
They made no commercial or external provider call:

- `tmp/terminal-evidence/tool-cards-export-20260911T120233Z-eb4b98` retains seven localhost requests,
  read and two writes, a denied shell that did not execute, one durable user
  message, collapsed/expanded long Bash output, and actual mouse
  expand/collapse. The pending Read and Bash permission panels, rejected state,
  success group and expanded retained tail were visually inspected.
- `tmp/terminal-evidence/tool-transcript-export-20260911T120308Z-b59adc` retains grouped Read+Glob,
  missing-root Glob failure, actual mouse expansion, PageDown traversal through
  all 25 read lines, and the visible negative retired-tool result
  `unknown tool: todo_write`. This forced fixture call was deliberately absent
  from the advertised 62-tool configured request and is negative execution
  evidence only, not a claim that a normal model can select the removed tool.
- `tmp/terminal-evidence/tool-transcript-focus-20260911T121848Z-d4c353` retains the same production
  transcript with actual Read and Edit plus Glob/Bash successes and failures.
  Focused mode preserves the user request, final answer and exact condensed
  outcome `Edited 1 file +1 -1, read 1 file, ran 4 other tools, 2 failed`; the
  second `/focus` restores the full transcript. Both toggles made zero provider
  requests.

The first journey confirms that the known generic approval and dense expanded
arguments remain visible differences from Claude; these are not silently
declared parity. The second closes the otherwise missing UI proof that the
retired TodoWrite name cannot execute while preserving structured Task
ownership. The third closes the mixed-tool summary and lossless restoration
evidence needed by the independent `P2-C-focus` command row; it does not close
the remaining tool rows in this audit.

## Focused filesystem and file-tool reconciliation

The retained core/B1 configured runs, no-cost native captures, controlled
contracts and current acceptance text were audited independently for the five
filesystem rows and five corresponding core tools. The result is deliberately
row-specific: **none of these ten rows is ready for `complete`**, so this pass
does not change `docs/terminal-compatibility-tracker.json`.

| Row | Highest accepted evidence | Exact states still open | Disposition |
| --- | --- | --- | --- |
| `P2-FS02` | Twenty-one edge regressions plus `advanced-files-search-visible-pty` cover bounded traversal, Unicode, hostile/oversized input accounting, unreadable descendants, cancellation and visible partial-search notice. | Through the configured model/TUI: denied and missing root, unreadable descendant, partial traversal, oversized/invalid text, symlink/root boundary and cancellation, each with actionable safe detail; real-repository bounded search; keyboard/mouse, modes and retained-result replay. Add a Linux non-UTF-8 filename canary only if cross-platform coverage is claimed, otherwise record macOS unavailability. | Keep `in_progress`. |
| `P2-FS03` | Controlled paging plus `advanced-files-pty-20260910T132129Z-d9ab44/v6` prove the 200-line default, 437 total/200 returned/237 remaining, explicit continuation metadata, bounded serialization and mouse-expanded display. | Configured dshx/Claude-equivalent default, explicit offset/limit and continuation; UTF-8 giant line, binary, empty, out-of-range, stale revision and cancellation; pending/working/success/error, scroll/expand, narrow/wide, dark/light/no-color and replay. | Keep `validating`. |
| `P2-FS04` | Controlled budgets and `advanced-files-pty-20260910T133510Z-e4bdfe` prove per-file results and a complete batch receipt above 32,768 characters without the former serialization clipping. | Actual configured-model `read_many` selection and Claude availability/equivalent; mixed success/error with per-file offsets, per-file policy denial, file-count and aggregate cap/deferred cases, cancellation, context-budget receipt, grouping/scroll/modes and replay. | Keep `validating`. |
| `P2-FS05` | Atomicity, stale/missing-match refusal, preview and applied diff pass locally; `advanced-files-update-context-pty` visibly retains failed, preview and success cards. | Actual configured-model `multi_edit` and exact current Claude equivalent or captured absence; ordered multi-edit success, stale revision, ambiguous/missing match, later-edit rollback, dry-run, denial and cancellation; bounded/long diff, grouped interaction, all display modes and replay. | Keep `validating`. |
| `P2-FS06` | Controlled atomic Write plus native cards prove create-only refusal, create, explicit observed replacement, unchanged receipts, permission preservation and stale/new-target protection. | Configured pair for create, no-op and guarded replace; stale/replaced target, existing-target refusal, missing parent/invalid path, permission denial and cancellation; verify targeted-edit schema guidance, exact preview/result, long/grouped/expanded states, modes and replay. | Keep `validating`. |
| `P2-T-Read` | Core and B1 each retain configured paired affirmative reads; dshx has exact schema, invocation, revision succession and read-before-Edit causality. | Configured paired continuation/range/batch, empty/missing/binary/long/stale/denied/cancelled states; rich notebook/attachment/document mapping; grouped expansion/scroll, all viewport/color modes and replay. Claude's exact outbound definition is still unavailable. | Keep `validating`. |
| `P2-T-Write` | Safe create/replace contracts and local pending/failure/success cards exist; the retained configured pilots did not invoke Write. | Current configured dshx/Claude selection for create/no-op/replace, stale/existing/missing-parent/invalid/denied/cancelled outcomes; exact preview/result, long/grouped/expanded interaction, modes and replay, including Claude availability/schema. | Keep `validating`. |
| `P2-T-Edit` | Core and B1 retain two configured paired affirmative edits with unchanged pending files, exact diffs, dshx revision carry and identical final bytes; no-cost v6 adds accept/deny/Escape/no-color behavior. | Missing/ambiguous match, stale, no-op/dry-run, cancellation, grouped multi-edit and long/truncated diff; final refined header/margins/colors, narrow/wide, dark/light/no-color, keyboard/mouse and replay. Claude's exact outbound definition remains unavailable. | Keep `validating`. |
| `P2-T-Glob` | Core retains actual configured dshx `glob` and Claude `Glob` calls with identical four-file discovery; controlled tests cover global ordering, cap policy, ignored paths, partial traversal, symlink policy and cancellation. | Configured empty, capped/partial, denied-root, unreadable and cancelled states; grouped/expanded interaction, viewports/themes/no-color and settled replay. Claude's actual call/result exists but its outbound definition does not. | Keep `validating`. |
| `P2-T-Grep` | Core retains actual configured calls/results in both products; current dshx adds files/content/count modes, path-aware glob, context, case-insensitivity, line-number control and pagination while preserving bounded stable search, binary/oversized accounting, cancellation and partial notice. | Claude's exact outbound definition remains unavailable, and dshx intentionally does not advertise `type`, `multiline`, or `only_matching`. Run the reviewed shared prompt for configured empty/no-match and capped content, then prefix-collision, long/partial, binary-skip, denied/error/cancel states plus grouping/expansion, modes and replay. | Keep `validating`. |

No common gate was used to blanket-fail or blanket-pass these rows. Each remains
open because at least one requirement in its own acceptance text lacks retained
evidence; the configured affirmative core path is not substituted for omitted
negative, interaction or durability states.

## Evidence rule used in this audit

For each applicable row, completion still requires all of the following unless
the row-specific entry narrows the requirement:

- **A — current availability:** actual selected model request records owner,
  registered service, configured prerequisite, compatible route, permission
  eligibility, advertised/deferred state, exact schema, and a correlated
  successful invocation or actionable unavailable reason;
- **P — paired presentation:** current Claude and current dshx at the same
  viewport and equivalent operation state, comparing title, summary,
  indentation, spacing, symbols, colors, arguments, status, and truthful
  result content;
- **L — lifecycle:** every applicable pending/queued, working/streaming,
  success, empty, error, denial, cancellation, grouped/concurrent,
  long/truncated, expanded, and collapsed state;
- **I — interaction/layout:** actual keyboard and mouse selection, expansion,
  scrolling, dismissal and owner return, with narrow and wide viewports plus
  dark, light, and no-color modes; drafts and input routing remain attached to
  the correct conversation; and
- **R — durability:** where the feature is durable, restart/replay recreates
  the same settled state without duplicate execution or a model request.

If Claude does not expose a referenced tool on the selected configured model,
capture the actual absence and do not substitute Bash, an MCP tool, a slash
command, or another domain. Per the original acceptance text, the paired
source state then remains open rather than being inferred.

## Foundation rows

| Row | Evidence already sufficient | Exact remaining gate | Shared scenario |
| --- | --- | --- | --- |
| `P2-FS02` | Complete exec/tools regressions, 21 edge tests, and the actual local `advanced-files-search-visible-pty` journey cover bounded scanning, Unicode output, skipped hostile data, unreadable descendants, cancellation, and a visible partial-search notice. | Show actionable error class and safe detail through the current configured model/TUI for denied root, missing root, unreadable descendant, partial traversal, oversized/invalid text, symlink/root boundary, and cancellation. Retain a real-repository bounded search. The non-UTF-8 filename case still needs a Linux canary if cross-platform coverage is claimed; otherwise record it as unavailable on the tested macOS filesystem. Complete the outstanding edge-audit inventory instead of treating the broad unit suite as proof that every UI error is understandable. P/L/I; R for retained results. | `S1` |
| `P2-TL01` | `/tools [name] [json]` is read-only and has controlled ownership, prepared-request, native-route and successful-call correlation tests. Wrapped inspector text has real local PTY evidence. The core pilot retains the complete dshx definitions and native routes from each of six same-turn request headers and correlates five exercised client tools. | Reconcile this current inventory across every validating row and capture later substantive scenarios after their prerequisites are configured. For every listed row, distinguish registered, configured, route-compatible, permission-eligible, advertised/deferred and actually exercised; prove stale prepared requests never describe the next request. Claude's normal credential-safe CLI supplied actual calls but no exact outbound definitions, so retain that reference gap rather than infer its schema. Exercise unavailable and restored prerequisites, then compare the inspector or closest truthful Claude surface with keyboard/mouse and viewport/theme coverage. A/P/L/I; R only for historical request attribution. | Existing `S0` readiness and core-pilot inventory, then every substantive scenario's request/post-run inspection |
| `P2-WS01` | Production composition tests prove local `WebFetch` is separate, native search is model-scoped, supported models receive it and unsupported models do not. An older request selected `openrouter:web_search`; that was route selection, not execution. | Invoke native search through the actual saved dshx route and current Claude route; retain the outgoing native feature declaration, provider response, normalized citations, usage and UI. Complete capability gating for OpenAI/Codex subscription, Anthropic, OpenRouter and xAI, including explicit unsupported/disabled reasons. One successful OpenRouter-model call cannot certify the other routes. A/P/L/I plus provider-specific usage and citation correctness. | `S8` plus its substantive request inventory; reuse existing `S0` readiness evidence |

## Per-row remaining gates

### Files, local content, and user-facing output

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-Artifact` | Bounded revision-checked local register/list/preview/remove behavior and retirement tests. It truthfully does not publish or deploy. | Actual configured-model schema and invocation; register, list, preview, stale revision, remove, missing handle and replay states; paired compact/expanded UI and interactions. Establish whether current Claude exposes an Artifact equivalent. If only a hosted publisher exists, retain the local-equivalent disposition and keep hosted publishing as a separately authorized integration rather than simulating it. A/P/L/I/R. | `S1` |
| `P2-T-Edit` | Read-before-write, stale refusal, exact and replace-all mutation, atomicity, diff context and provider dispatch pass. The current core pilot pairs real configured approval and success on the same fixture; both changed only `src/config.txt`, and dshx passed the exact schema plus revision-carry assertion. B1 adds a second configured pair whose pending cards both show the exact one-line `BEFORE` → `AFTER` diff while files remain unchanged. | Capture deny, missing/ambiguous match, stale revision, dry-run/unchanged, cancellation, grouped multi-edit and long/truncated diff. Verify final header, margins and colors after the post-capture refinements at narrow/wide and dark/light/no-color; complete keyboard/mouse expansion and replay coverage. Current source corrects the frozen v7 single-Accept helper; rebuilt capture pending. Claude's exact outbound definition remains unavailable even though its actual Edit calls/results are retained. P/L/I/R plus the remaining Claude A-schema gap. | Core and B1 affirmative paths observed; remaining `S1` clones |
| `P2-T-Glob` | Global lexical ordering before cap, ignored/vendor handling, partial traversal, cancellation and symlink policy pass. The core pilot now adds actual current dshx `glob` and Claude `Glob` calls on cloned fixtures with the same four-file discovery outcome; no shell substitute was used. | Run empty, cap/partial notice, denied root, unreadable descendant and cancellation through the configured model. Claude's actual call/result is retained, but its exact outbound definition remains unavailable. Compare grouped and expanded output plus themes/viewports and settled replay. P/L/I/R plus the remaining Claude A-schema gap. | Core part of `S1` observed; remaining `S1` clones |
| `P2-T-Grep` | Regex/include filtering, bounded excerpts, binary and oversized-line accounting, stable traversal, cancellation, provider dispatch and visible partial notice pass locally. The core pilot adds current dshx `grep` and Claude `Grep` calls. Current dshx now reconciles the observed output-mode/line-number shape and implements bounded files/content/count, glob, context, case-insensitivity and pagination with strict provider validation. | Claude's exact outbound definition remains unavailable. `type`, `multiline`, and `only_matching` are explicitly unsupported in dshx rather than silently approximated. Run the reviewed shared prompt for capped content and no-match, then multiple-file prefix collision, long-line/partial, binary skip, denied root, error and cancellation through configured models. P/L/I/R plus the remaining Claude A-schema gap. | Core part of `S1` observed; remaining `S1` clones |
| `P2-T-NotebookEdit` | Real nbformat 4 replace/insert/delete, metadata preservation, output clearing, revision/cell-id conflict, invalid cell, external change and cancellation pass controlled tests. Cell execution is correctly not claimed. | Current configured model must select the native notebook tool against the same notebook fixture in both products. Capture replace, insert, delete, stale/external-change, invalid-cell, denial and cancellation; verify exact card/diff, compact/expanded state, grouping and replay across viewport/theme/input methods. A/P/L/I/R. | `S1` |
| `P2-T-Read` | Default and explicit paging, continuation revisions, byte/line caps, UTF-8 long lines, binary refusal, batch budgets, provider dispatch and observation logging pass. The core pilot adds two actual reads per configured product, including the intended target before Edit; dshx retained its exact schema/revision and Claude retained actual calls/results. B1 adds paired plan-research and final-verification reads with exact revision succession. | Run continuation, explicit range, batch/deferred budget, empty file, missing file, binary refusal, long/truncation, stale continuation, denied root and cancellation through the configured pair. Resolve the rich-file discoverability mapping for notebook/attachment/document inputs instead of assuming filesystem Read covers them. Current source corrects the frozen v7 single-Accept helper; rebuilt capture pending. Compare grouping, expansion, scroll, themes, viewports and replay; retain Claude's outbound-definition gap. P/L/I/R plus remaining A. | Core and B1 affirmative paths observed; remaining `S1` clones |
| `P2-T-SendUserFile` | The real local CLI/ATT01 journey covers approval, exact ordered bytes, success, expansion, save after source mutation/deletion, admission refusal, replay without inference and missing-object save failure. Remote attempts were zero by design. | Current configured dshx model selection and a current Claude tool availability/schema capture. If exposed by Claude, compare equivalent local file cards for pending/working/success/refusal/cancel, one/many files, long names, expanded metadata and save action across input methods and modes. Remote/mobile delivery remains outside this local contract and must not be claimed or tested without a client service and destination authorization. A/P/L/I/R. | `S1` |
| `P2-T-Write` | Safe create, parent creation, explicit observed replacement, stale/new-target refusal, unchanged receipt, provider dispatch and atomic filesystem behavior pass. Claude approval/completed and local controlled cards exist but are not a complete current pair. | Current configured pair for create, identical no-op, explicit replace, stale revision, existing-target refusal, missing parent/invalid path, denied permission and cancellation. Compare exact preview/result, long content/truncation, grouped state, expansion, keyboard/mouse and viewport/theme modes. A/P/L/I/R. | `S1` |

### Human interaction, plan, and workspace transition

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-AskUserQuestion` | Blocking/async infrastructure, pending durability, reopen, cancellation and exact-once answer pass controlled tests. B1 adds actual configured calls, matched pending presentation and one exact Beta answer in both products. | Capture Alpha, free text, user cancellation, process restart while pending, resumed/async answer and duplicate-answer refusal. Resolve the observed alternatives/wrapping differences and verify transcript placement, focus/draft restoration, keyboard/mouse and display modes. P/L/I/R; Claude definition remains unavailable. | B1 affirmative path observed; remaining `S3` clones |
| `P2-T-EnterPlanMode` | Durable idempotent effect ownership and same-batch mutation guard pass. B1 adds actual configured advertisement, selection and success in both products. | Capture reentry, a later same-batch mutation refusal, interruption and restart through the configured pair. Classify the extra native entry approval as an intentional policy or add an explicit non-escalating session-transition effect; do not mislabel the durable mutation side-effect-free. Current source adds the typed completed summary; rebuilt capture pending. Verify status/footer/input routing at narrow/wide and all color modes. P/L/I/R; Claude definition remains unavailable. | B1 affirmative path observed; remaining `S3` clones |
| `P2-T-ExitPlanMode` | Proposal/review/feedback/accept/reject/cancel/reopen tests pass; the local PTY covers five decision branches. B1 adds one configured full-plan/default-policy acceptance per product with no edit before acceptance and a durable native `default` decision. | Pair accept-with-edits, feedback/stay, escape/cancel, rejection and failed-application states through configured clones; verify durable mode/decision after restart. Preserve the safer stay-in-Plan default unless product policy explicitly changes; improve navigation/mouse/theme behavior instead. Current source adds the typed completed summary; rebuilt capture pending. Exercise mouse/keyboard and preserve drafts. P/L/I/R; Claude definition remains unavailable. | B1 affirmative path observed; remaining `S3` clones |
| `P2-T-EnterWorktree` | Production model loop, Git snapshot, rebinding, instructions, terminals, fences and recovery tests pass locally. | Current configured tool invocation, actual current Claude tool exposure and equivalent entry state. Pair approval, entered cwd/root/instruction state, dirty/untracked preservation, restricted-mode refusal, busy-owner refusal, failed/pending recovery and restart. The existing Claude `/add-dir` command capture is not an EnterWorktree tool reference. A/P/L/I/R. | `S7` |
| `P2-T-ExitWorktree` | Exact previous authority restoration and deliberate retained-worktree behavior pass controlled production tests; no merge/delete is claimed. | Current configured tool invocation and Claude availability, paired exit success, not-in-worktree error, busy/pending-recovery refusal and restart/reopen. Verify the result says retained and never implies merged/deleted; inspect restored file/shell/instruction scope plus keyboard/mouse and display modes. A/P/L/I/R. | `S7` |

### Schedules

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-CronCreate` | Five-field local cron, local timezone, deterministic jitter, seven-day expiry, bounded admission, durable replay and a real local CLI create/list/restart path pass. One real local one-shot timer fired exactly once. | Actual configured-model schema and create invocation; authenticated/current Claude availability rather than the prompt-free unauthenticated picker. Pair valid recurring/one-shot creation, invalid cron, cap/expiry, pending/working/approval/error and owner-scoped restart. A recurring due-fire remains terminal-timing evidence only in controlled tests and should be captured if recurrence execution, not just admission, is claimed live. A/P/L/I/R. | `S5` |
| `P2-T-CronDelete` | Controlled race coverage settles an admitted unclaimed occurrence; local CLI deletion survives restart. | Configured-model delete, not-found/foreign/already-fired outcomes, and the already-admitted race through a real terminal state. Pair Claude if exposed; otherwise capture absence and leave the source gate open. Show cancelled/settled state and no later duplicate dispatch after restart. A/P/L/I/R. | `S5` |
| `P2-T-CronList` | Local receipts expose expression, rule, timezone, next run, state, owner, expiry, jitter and resume behavior; local CLI list before/after restart passes. | Configured-model list with empty, one-shot, recurring, expired/deleted and multiple-record states; exact current Claude schema/UI if exposed. Verify pagination/bounds if the list exceeds one page, expansion/scroll and all display/input modes. A/P/L/I/R. | `S5` |
| `P2-T-ScheduleWakeup` | Explicit create/reschedule/stop and settlement pass; local PTY shows the operation sequence and no restored stopped wakeup. | Configured-model create/reschedule/stop and current Claude availability. Pair awaiting-decision, rescheduled, stopped, expired/invalid and cancellation-race states. The one-minute wakeup fire itself remains controlled-test evidence, and dshx intentionally has no implicit roughly-20-minute fallback or resume restoration; preserve that semantic difference. A/P/L/I; R must prove non-restoration/settlement, not resurrection. | `S5` |

### MCP, LSP, and skills

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-ListMcpResourcesTool` | Exact stdio fixtures cover bounded listing, pagination, stale generation, identifiers, exposure, zero-server availability and teardown. A real isolated XcodeBuildMCP 2.7.0 canary listed four resources. | Actual configured-model invocation against the same local server in each product. Pair zero-server, connecting, ready, paginated, empty, failed/auth-required, disappearing and stale-continuation states; retain exact request schema and untrusted provenance. Exercise scroll/expand, keyboard/mouse and display modes. Broader third-party servers remain a compatibility rather than pixel-parity matrix. A/P/L/I/R. | `S6` |
| `P2-T-ReadMcpResourceTool` | Ready/exposure gating, exact committed URI, 64 KiB UTF-8 preview, binary omission, hostile identifier, cancellation, body-free transport errors and teardown pass. The XcodeBuildMCP canary read inert JSON. | Current configured-model invocation for text, binary metadata, long/truncated text, URI not in current generation, server failure/auth, cancellation and disappeared connection. Pair current Claude on the same server and verify no hostile body leaks into errors. A/P/L/I/R. | `S6` |
| `P2-T-WaitForMcpServers` | Controlled ready, no-server, timeout, terminal settlement, disappearance, selected-id, cancellation and teardown pass; XcodeBuildMCP reached `all_ready`. | Current configured-model invocation with a deliberately slow local server plus no-server, ready, timed-out, failed/auth-required and disappearing states. Compare current Claude semantics if it exposes a wait tool; a reconnect or status command is not a substitute. Pair progress/cancel/results and interaction/display modes. A/P/L/I/R where state is durable. | `S6` |
| `P2-T-LSP` | Canonical ten-operation tool passes an exact stdio model boundary. Apple clangd 17 passes eight applicable operations; outgoing calls return a recoverable server-unsupported error. | Actual configured-model selection/invocation; current Claude LSP availability; paired cards for server list, definition/references/hover/symbols/calls/diagnostics, empty diagnostics, malformed/out-of-root input, unsupported operation, long retained output and cancellation. Add representative rust-analyzer, pyright and TypeScript-server canaries (or explicitly narrow the support claim) because one Apple clangd build is not a production-server matrix. A/P/L/I/R. | `S6` |
| `P2-T-Skill` | Independent bounded model-facing discovery/loading and registry tests pass; command-side skill panels do not prove the model tool. | Current configured request advertises the exact skill-loading surface and the model invokes a known fixture skill. Pair valid, unknown, malformed/oversized, removed-after-discovery and restart cases; verify source attribution, untrusted text boundaries, card/error/expansion and display/input modes. Determine the actual current Claude equivalent rather than equating `/skills` with the model tool. A/P/L/I/R. | `S6` |

### Review, workflow, and monitoring

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-ReportFindings` | Real local dshx CLI/reviewer path covers exact revisions, accepted grouped result, expansion, stale refusal and provider-free replay. A current Claude 2.1.268 localhost-fixture success card and exact schema were captured. | Run an unscripted configured model on the same defect in both products. Complete multi-file/multi-severity grouping, empty report if applicable, invalid/stale failure, denial, cancellation, long/32-item truncation, expanded/collapsed and replay matrix. Resolve or explicitly accept the known presentation mismatches: dense raw dshx approval JSON, misleading generic approval helper copy, and stale error discoverable only in the child. Claude's schema lacks dshx revision/source authority, so do not fabricate a paired stale state. A/P/L/I/R. | `S7` |
| `P2-T-Monitor` | Filtered/deduplicated delivery, cancellation and source ownership pass controlled tests. | Define the exact Claude-equivalent source families first. Invoke the current configured dshx tool for bounded live events, duplicate suppression, filter/no-match, source end, timeout, cancellation and foreign-source refusal; pair only source types Claude actually exposes. Capture streaming/working, final settlement, grouping, long output, replay and owner-correct interaction. A/P/L/I/R. | `S2` |
| `P2-T-Workflow` | The opt-in local `workflow` surface has controlled progress, cancellation, checkpoint/resume and a real localhost PTY with expansion/collapse. Agent, Work and Workflow identities remain separate. | Excluding `run_code` and code mode, invoke `workflow` through the saved configured dshx model. Establish the truthful current Claude equivalent or record none; do not relabel an agent/task as a workflow. Pair progress, success, failure, cancellation, checkpoint/resume, grouped/long output, expansion and Work/Agent separation across interaction/display modes. A/P/L/I/R. | `S7` |

### Agents, process output, and structured Work

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-Bash` | Foreground execution, timeout/tree teardown, non-zero results, bounded visible tail, retained output, background promotion and terminal ownership have controlled coverage. The core pilot adds a current paired approval→running→success call: each product ran exactly `python3 checks/check.py` once and returned `CORE_CHECK_OK`; expanded/collapsed tool views and mouse shell-group expansion were retained. | Complete deny, no output, non-zero, timeout, interruption, long/truncated output, durable replay and background calls with two isolated shells, paging and typed stop. Pair shell list/detail/back/foreground behavior with keyboard/mouse and every viewport/theme mode while preserving the parent draft. Claude's exact outbound Bash definition remains unavailable. P/L/I/R plus remaining A. | Foreground core part of `S2` observed; remaining `S2` clones |
| `P2-T-ListAgents` | Native owner-scoped lifecycle/IDs/timing/output tests and the final five-agent local terminal matrix pass. Claude `/tasks` and child follow-up captures exist, but not a paired `ListAgents` model-call receipt. | Current configured model must call agent-only inventory with 0, 1, 5 and 20 children spanning running/waiting/idle/completed/failed/cancelled/unread. Capture exact Claude tool availability and prevent shell jobs/Work items from leaking into results. Pair paging/long names, expansion, navigation, follow-up return, keyboard/mouse and display modes. A/P/L/I/R. | `S4` |
| `P2-T-SendMessage` | Scoped child authority, durable receipts and exact-once busy/idle steer consumption pass integration tests; existing navigation proves local follow-up continuity. | Configured-model calls to a busy child, idle continuable child, stopped child, unknown/foreign id and duplicate/replayed message. Pair Claude target selection and receipt/pending/delivery/error states; prove a stopped child is not silently treated as live and cross-session transport remains separate. Verify owner return, unread state and input routing with keyboard/mouse/themes. A/P/L/I/R. | `S4` |
| `P2-T-TaskCreate` | Core Work/domain tests, team adapter and the 30-test task-console slice cover idempotency, dependencies, scope, durable projection and Work-only read-only presentation. | Current configured model creates stable scoped items with subject/details/owner/dependencies/metadata; repeat the same client id after later edits and prove no duplicate. Pair Claude current schema/card for success, invalid dependency/owner, denial if applicable, concurrent/grouped creation and restart. A/P/L/I/R. | `S4` |
| `P2-T-TaskGet` | Stable lookup, paging domain and durable board are covered by controlled Work tests. | Configured-model get for live item, updated revision, tombstone, unknown and foreign-scope id. Pair compact/detail rendering, long fields, error and replay with keyboard/mouse and display modes. A/P/L/I/R. | `S4` |
| `P2-T-TaskList` | Controlled Work projection keeps Work distinct from agents/jobs and supports paging. The current 30-test console slice keeps Work out of running-execution counts and agent/process affordances while preserving keyboard/mouse detail and return. | Configured-model lists empty and populated boards, multiple simultaneous active items, blocked/ready/deleted states, filters and pagination before/after restart. Pair current Claude if exposed and assert no shell process or agent conversation appears. Exercise list selection/scroll/detail/return across modes. A/P/L/I/R. | `S4` |
| `P2-T-TaskOutput` | `job_output` is a real retained process-output service, child transcripts are independently owned, and local terminal detail/output isolation passes. | Current configured output retrieval while running and after success/failure/cancel, with at least two pages, exact continuation/offset, truncation, missing/foreign id and replay. Establish whether current Claude exposes TaskOutput; do not treat agent navigation as the tool. Pair merged/reference UI and interactions. A/P/L/I/R. | `S2` and `S4` |
| `P2-T-TaskStop` | Typed agent interrupt, process cancellation and terminal kill remain separate; controlled tests cover stop-before-poll, child-tool teardown and cancelled settlement. | Current configured calls for each exact domain, including an agent and process with confusable numeric labels, stopping→settled transition, already-settled, not-found and foreign target. Pair only the actual Claude target domain; prove no ID collision stops the wrong owner. Capture cancellation, teardown, replay, keyboard/mouse and display modes. A/P/L/I/R. | `S2` and `S4` |
| `P2-T-TaskUpdate` | Controlled Work tests cover per-item CAS, independent updates, dependencies, assignments, deletion/tombstones and team ownership. | Through current configured models, update two different items concurrently without loss; reject a stale revision and dependency cycle atomically; exercise details/status/assignment/dependencies/delete and team authority. Pair Claude schema/results and Work UI for success, blocked, conflict, invalid/foreign and tombstone states before/after restart. A/P/L/I/R. | `S4` |
| `P2-T-TodoWrite` | Current composition tests say new dshx requests omit TodoWrite and successful legacy results migrate into the Work projection without rewriting journals. | Retain a fresh configured dshx request proving `todo_write` absent while `task_create/get/list/update` are present, and show the model cannot call the removed name. Record whether the selected Claude model exposes TodoWrite or structured Task tools. Reopen a legacy todo session and inspect its read-only migration beside current Work UI with no duplicate mutation. There is no new TodoWrite-call UI to compare on dshx; its remaining presentation dependency is the structured Task family. A/R plus `S4` P/L/I. | `S4` request inventory; reuse existing `S0` readiness evidence |

### Web

| Row | Evidence already sufficient | Exact missing gates | Shared scenario |
| --- | --- | --- | --- |
| `P2-T-WebFetch` | Controlled registry dispatch, HTML/PDF extraction, source metadata, redirect and SSRF/DNS policy, cancellation and UTF-8 bounds pass. | Current configured-model invocation on the same public HTML and PDF sources in both products, retaining exact URL/source metadata and any citations. Pair normal, empty/unreadable, redirect, blocked private address, transport error, cancellation and long/truncated output; inspect route/prerequisite/permission, UI expansion and modes. This future run is an authorized external network canary, unlike existing controlled tests. A/P/L/I; R for settled transcript. | `S8` |
| `P2-T-WebSearch` | Portable/native routing and policy tests pass; prior request route selection is known. | Same as `P2-WS01`: actual search on the saved dshx model and Claude, direct citations and usage, plus the complete OpenAI/Codex, Anthropic, OpenRouter and xAI gating matrix. Pair success, zero-result if reproducible, provider error, cancellation, long results and citation expansion. A/P/L/I plus provider route/usage evidence. | `S8` |

## Proposed bounded configured-model matrix

The following is a nine-scenario first pass, not permission to execute and not
the complete all-state closure matrix. `S0` is already complete as a readiness
probe and must not be repeated. Run each substantive baseline once on Claude
Opus 5 and once on saved dshx `meta/muse-spark-1.3` in cloned disposable fixtures.
Use the same 100×45 dark terminal first. Pending approvals can be forked before
a decision to capture allow, deny and cancel without repeating the prompt;
settled sessions can be reopened/resized/rethemed without another model call.
Only uncovered row-specific states should trigger an additional prompt.

For every run retain: exact version/model/effort/route/permission mode,
fixture manifest and hashes, outbound request with tool/native-feature schema,
provider response, durable journal, raw ANSI, decoded cells/text, PNGs,
keyboard/mouse actions, external-request destinations and usage. A generic
fallback or an absent tool is recorded, never counted as the requested row.

### `S0` — retained readiness; schema inventory piggybacks the first real task

Do not send another readiness prompt. Reuse
`tmp/terminal-evidence/live-model-readiness-20260911T105718Z-eb556a`: each product returned exactly
`READY`, and both recorded zero tool use. This is access evidence only.

On each product's first substantive scenario, capture the exact outbound client
tool schemas and provider-native feature declarations actually offered to that
model. Reconcile `task_*`, removed `todo_write`, MCP/LSP/Skill prerequisites,
`SendUserFile`, plan/worktree tools, and native web search with
`/tools <name> json` afterward. An omitted tool gets an exact stage/reason, not
an inferred failure. This piggybacked inventory, not the readiness request, is
the remaining `S0` schema evidence. The shared core pilot has now captured this
for dshx; Claude's normal CLI retains actual calls but exposes no equivalent
credential-safe exact outbound definition artifact, so that source gap stays
explicit.

### `S1` — files, search, notebook, artifact, and local delivery

Fixture: `fixture/base.txt` contains `alpha\nbeta\nNEEDLE\n`; a >200-line UTF-8
file, an empty file, a binary file, an nbformat 4 notebook, a read-only/outside
path fixture where supported, and a capped directory tree with lexical
component/string prefix collisions.

Exact baseline prompt:

```text
Within this disposable workspace only, use native tools and never shell
substitutes. In order: Glob fixture/**/*; Grep NEEDLE; Read fixture/base.txt;
create fixture/new.txt containing exactly alpha, beta, gamma and a final
newline; edit beta to delta; replace the first notebook cell with
print("MATRIX_NOTEBOOK"); register and preview fixture/new.txt as a local
artifact; then make fixture/new.txt available in this conversation. If a
named tool family is unavailable, write UNAVAILABLE:<family> and do not
substitute another tool. Finish with FILE_MATRIX_DONE.
```

Expected inspectable outcomes: actual named families in the request/calls;
ordered grouped success with truthful mutation previews; a local-only delivery
receipt, not a remote claim. Forked fixture/session states cover deny/cancel,
missing/binary/empty/long reads, stale and ambiguous edit, existing-target
write, partial search and attachment-object loss. Reopen after source mutation
and deletion to separate path provenance from retained bytes.

### `S2` — shell, retained output, monitor, and typed stop

Exact baseline prompt:

```text
Use native shell and process tools only. Run printf 'FG_OK\n' in the
foreground. Start one background process that prints BG_01 through BG_40 with
a short delay between lines. Read its output in two non-overlapping pages,
observe one supported event from that exact process, stop that exact process,
and report its settled state. Do not target or stop an agent. Finish with
SHELL_MATRIX_DONE.
```

Expected inspectable outcomes: foreground pending/working/success; background
handoff with stable identity; non-overlapping retained pages; source-owned
monitor event; stopping→settled transition on the process only. Clone for
denial, empty output, non-zero, timeout, interruption, not-found and two
simultaneous shells with isolated output.

### `S3` — question and plan lifecycle

Exact question prompt:

```text
Use AskUserQuestion once to ask "Choose the matrix branch" with exactly the
choices "Alpha" and "Beta". Do nothing else until I answer.
```

Answer `Beta` in one clone, cancel in another, and terminate/reopen while
pending in a third. Expected outcomes: one pending question, one exact answer
consumed once, an explicit cancelled state, and a resumed pending state without
duplicate inference.

Exact plan prompt:

```text
Enter plan mode using the model-facing plan tool. Propose changing only
fixture/plan.txt from BEFORE to AFTER, then request exit from plan mode. Do not
edit the file before I accept the plan.
```

Expected outcomes: native entry call, durable active plan, mutation guard,
review/exit UI and no early edit. Fork the review decision for accept,
accept-with-edits/default, feedback/stay, reject and escape/cancel; reopen each
settled state.

### `S4` — structured Work and agent messaging

Exact baseline prompt:

```text
Use structured Task tools only and never TodoWrite. Create work item A, work
item B blocked by A, and independent work item C, each with a stable client id.
List the board, get B by id, make A and C active, complete A, and verify that B
is now ready. Finish with WORK_MATRIX_CREATED.
```

Expected outcomes: stable IDs/revisions, three Work items only, multiple active
items, correct dependency readiness and no agent/job leakage. Repeat A's create
client id after its update; then attempt one stale update and one dependency
cycle, both without partial mutation.

Agent continuation prompt after creating two tiny continuable reader agents:

```text
List only the agent conversations. Send BUSY_FOLLOWUP to the running agent and
IDLE_FOLLOWUP to the idle continuable agent, then stop only the first agent.
Do not stop a shell process and do not alter any Work item. Finish with
AGENT_MATRIX_DONE.
```

Expected outcomes: owner-scoped inventory, exact-once busy/idle delivery,
unread/settled transitions and typed stop. Add unknown/foreign/stopped targets,
0/1/5/20 inventory, long names, paged output and restart without another
creation. Two agents update different Work items concurrently to satisfy the
original no-lost-update acceptance with a real configured path.

### `S5` — schedule family

Exact baseline prompt:

```text
Using native scheduling tools only, create one one-shot schedule for 90
seconds from now with prompt SCHED_MATRIX_ONCE and one recurring cron
17 * * * * with prompt SCHED_MATRIX_CRON in the local timezone. List the exact
next run, state, timezone, owner, expiry and jitter for both. Do not wait for a
timer. Finish with SCHEDULE_MATRIX_CREATED.
```

Expected outcomes: exact native schedule schemas/calls, stable ids and truthful
one-shot/recurring metadata. Delete both before due and verify empty before and
after restart. In a separate clone create a 60-second self-paced wakeup,
reschedule it to 120 seconds and stop it. If Claude omits these tool families,
capture the configured request absence and do not invoke a hosted command.

### `S6` — MCP, LSP, and Skill prerequisites

Use the same local slow/paginated MCP fixture, Apple clangd fixture, and a
minimal `matrix-skill` in both products where each product supports that
configuration.

Exact prompts:

```text
Wait up to 5 seconds for matrix-mcp, list its resources one item per page until
the continuation ends, read matrix://text/one, then attempt the advertised
binary resource. Finish with MCP_MATRIX_DONE.
```

```text
Using LSP only, report definition, references, hover, document symbols,
workspace symbols, incoming calls, outgoing calls and diagnostics for
fixture/main.cpp. Finish with LSP_MATRIX_DONE.
```

```text
Load and follow the skill named matrix-skill. Finish with SKILL_MATRIX_DONE.
```

Expected outcomes: real prerequisite transitions and exact tool calls, paged
untrusted MCP data, bounded resource read, truthful server-unsupported LSP
operation and attributed skill body. Clone MCP timeout/auth-failed/disappeared,
stale URI and cancellation; LSP absent/out-of-root/long output; and missing or
removed skill. Do not treat slash-command panels as model-tool execution.

### `S7` — worktree, findings, and Workflow

Use three cloned sessions so workspace transition, review policy and workflow
state cannot contaminate one another.

Exact prompts:

```text
Enter a managed worktree with the model-facing tool, create
fixture/worktree-marker.txt containing exactly WORKTREE_ONLY and a final
newline, then exit with the model-facing tool. Do not merge or delete the
worktree. Finish with WORKTREE_MATRIX_DONE.
```

```text
Review fixture/review.py and report exactly the real indexed-access defect
using the native structured findings tool. Do not edit the file. Finish with
REVIEW_MATRIX_DONE.
```

```text
Run the named matrix workflow using only the workflow tool. Do not use
run_code or code mode. Report its progress and durable checkpoint, then finish
with WORKFLOW_MATRIX_DONE.
```

Expected outcomes: scoped entry/exit and retained worktree; one source-backed
finding with current revision and grouped UI; one independently identified
Workflow. Clone worktree refusal/recovery, stale finding/deny/cancel/multi-file
grouping, and workflow failure/cancel/restart.

### `S8` — native web search and local WebFetch

Exact prompts:

```text
Using native WebSearch only, find the official OpenAI Models documentation
page and return its exact title plus one direct citation. Do not substitute
WebFetch or a browser. Finish with WEB_SEARCH_MATRIX_DONE.
```

```text
Using WebFetch only, fetch that exact official URL and report its title,
source URL, and whether the returned content was truncated. Do not perform a
new search. Finish with WEB_FETCH_MATRIX_DONE.
```

Expected inspectable outcomes: actual selected native route and feature,
provider result/usage, a direct source citation, and a separate bounded fetch
with source metadata. An unsupported provider must omit/disable native search
with an actionable reason. Clone blocked private URL, redirect, provider error,
cancellation and long/truncated content only after the baseline succeeds.

## Coverage and sequencing recommendation

Reuse the completed `S0` readiness evidence. Capture the still-missing schema
inventory on the first substantive request; it may prove that some Claude tool
rows cannot be reproduced on the selected model. Record those rows as
missing-reference gates without spending follow-up prompts on substitutions.
Run `S1`, `S2`, `S3`, `S4`, and `S8` first because they cover the core file,
process, interaction, Work, agent and web paths with the highest number of
validating rows. Run `S5`–`S7` only for tools actually advertised and
configured in the captured inventory.

The nine baseline scenario families require at most one initial prompt per
listed prompt block and product; most denial, cancellation, replay, resize,
theme and input-method evidence can be obtained by cloning or reopening state
without repeating inference. This is still a first pass. Every unobserved
applicable state remains open, and provider-family certification for `P2-WS01`
is a separate matrix beyond the single saved dshx route.

## Inspected evidence

- `docs/terminal-compatibility-tracker.json`
- `docs/terminal-compatibility-requirements.md`
- `docs/audits/tool-audit.md`
- `docs/audits/tool-ui-matrix.json`
- `docs/audits/real-compatibility.md`
- `docs/audits/scheduler-terminal-validation.md`
- `docs/audits/workspace-transition-audit.md`
- `docs/audits/workspace-terminal-evidence.md`
- `docs/audits/report-findings-audit.md`
- `docs/audits/report-findings-live-validation.md`
- `docs/audits/send-user-file-audit.md`
- `docs/audits/terminal-audit.md`
- `docs/audits/grouped-tool-transcript-2026-09-08.md`
- `docs/audits/tui-workflow-unlimited-execution-2026-09-08.md`
- `tmp/terminal-evidence/live-model-readiness-20260911T105718Z-eb556a/readiness-and-matrix.md`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/assertions.json`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/dshx-request-headers.json`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/dshx-tool-events.json`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/claude-tool-events.json`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/final-fixtures.json`
- `tmp/terminal-evidence/live-core-pilot-20260911T112442Z-d5adcf/both-edit-pending-files.json`
- machine-readable local PTY results under
  `tmp/terminal-evidence/advanced-files-refined-inspector-pty-20260910T135934Z-16e14b`,
  `scheduler-pty-20260911T093019Z-285a24`, `report-findings-pty-20260911T100820Z-4434a5`,
  `report-findings-claude-20260911T094609Z-9e9cc9`, `send-user-file-pty-20260911T104154Z-591a15`,
  `agent-background-activity-pty-20260911T015630Z-06fc21`, and the task-console dark/light
  captures.
