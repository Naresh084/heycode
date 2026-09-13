# Multiagent audit implementation tracker

> Superseded scope — 2026-09-08: at the user's request, the entire HTTP remote feature was removed, including the browser dashboard, remote host CLI, HTTP transport and webhook ingress. Historical remote commands, screenshots and test counts below describe the former implementation and are not shipping capabilities or current acceptance evidence. The terminal TUI, headless CLI and stdio AppServer remain, along with native schedules, inbox delivery, workflows and Plan review.

Source: [37-finding audit](../audits/multiagent-background-capability-audit-2026-09-07.md). Requested 2026-09-08: implement every gap end to end, with parallel Codex tasks using gpt-6-astra / high, prioritizing quality over elapsed time.

## Completion contract

A finding closes only when its shipping runtime path exists, its controls/documentation agree with it, relevant failure and lifecycle tests pass, and its user journey has been checked. A service, stub, schema or passing fixture alone is insufficient. Preserve all changes preceding this effort. No paid provider smoke test is required for deterministic correctness; external runtime readiness must be reported honestly.

## Progress update — 2026-09-08

All ten requested Codex tasks delivered implementations. The integrated result passes **4,479 tests with zero failures** across 200 targets, strict workspace lint, formatting, generated references and documentation checks. Final native terminal journeys passed for the workflow workspace (19 captures), task lifecycle (13 captures) and all four Plan-review decisions. The [closure report](../audits/native-agent-implementation-2026-09-08.md) links the implementation and evidence for every finding. Eleven optional/fixture tests were skipped by the combined invocation; separately executed browser/acoustic checks and paused human-dependent checks are identified in the report.

| Stream | Integrated behavior | Validation state |
|---|---|---|
| Runtime | Early stable task IDs, first-turn cancellation, parallel native calls, scoped worktrees, configurable shared admission limits, retained history, fork-and-continue, inbox steering and close/archive controls. | Native lifecycle, worktree preservation, sandbox, nested completion and restart regressions; combined workspace suite passed. |
| Custom agents | Full resolved settings, separate standing instructions, enforced tool/permission ceilings, imports/reload, scoped memory and native review/advisor/security commands. | Actual user override and fallback recovery verified; minimal intentional profiles remain supported. |
| Execution | Live bounded stdout/stderr/PTY tails, Monitor filters/wakes, general background tools, one-process promotion and joined cancellation. | Real terminal journey passed; shared subprocess lifetime regression fixed and affected exec/MCP suites pass. |
| Workflow / teams | Native tool/agent dependency graphs, result binding, bounded retry, durable save/pause/resume, ready-team bootstrap, dispatch and automatic mail delivery. | Actual caller tools/session/authority retained; ownership and team regressions pass. |
| Task UI | Persistent task strip, live child conversations, retained drafts, execution telemetry, controls and bounded provider readiness probes. | Integrated real PTY: 13 captures, three children, steering/close/cancel, terminal input, one-invocation promotion. |
| Plan | Full Markdown review, three typed decisions, atomic permission handoff, durable feedback and read-only gates including hooks/background work. | Integrated PTYs passed Accepted edits, Default, No with feedback and Escape with the full 42,255-byte proposal. |
| Provider parity | Native protocol/state continuation across 22 production adapter routes; explicit cross-provider fallback with independent credentials and prepared target activation. | Production adapter matrix and fallback regressions pass; combined workspace suite passed. |
| Current commands | Recap, side questions, asynchronous answers, conflict-aware write/edit rewind, styles and skill diagnostics. | Nested CodeMode checkpoint/question ownership and existing session/compaction regressions pass. |
| Remote / routines | Removed by user request on 2026-09-08. Native terminal schedules and inbox remain. | Earlier remote evidence is historical only; the remote implementation and smoke scripts were removed. |
| Browser / desktop / voice | Isolated browser, artifact previews, notebook edits, native macOS helper and configured local PCM transcription. | Real browser, child-workspace isolation and generated 16/48 kHz speech-to-composer recognition verified. Visible native GUI input is paused while the Mac is locked, at the user's instruction. |
| Root integration | Bounded JavaScript VM, current-agent guarded tool bridge, schema discovery, durable scripts and owner-scoped pause/resume/stop. | VM, durable journal, child ownership, checkpoint and live-control tests pass; combined workspace suite, strict lint, terminal journeys and closure report complete. |

The original closure claim predates the user-requested HTTP removal; detached remote control is now intentionally outside the product scope. The locked-Mac and personal microphone checks remain paused independently at the user’s instruction. No paid model inference or public deployment is required for these deterministic tests.

## Workstreams and ownership

| Workstream | Audit findings | Primary ownership | Status |
|---|---|---|---|
| Runtime lifecycle and isolation | 1, 4, 5, 6, 7, 10, 11, 12, 27, 28, 29, 35, 36 | Agent subagent registry/runner, jobs, worktree/runtime lifecycle | Complete; verified |
| Custom agent configuration | 23, 24, 25, 26 | Extension declarations, resolved preset settings, validation/import/reload | Complete; verified |
| Execution and monitoring | 2, 3, 8, 9 runtime, 19, 20, 21, 22 | Shell/PTY execution, retained output, Monitor, general background execution | Complete; verified |
| Workflow and team orchestration | 30, 31, 32, 33, 34 backend | Workflow worker/definition, team bootstrap/dispatch/mail | Complete; verified |
| Task and agent interface | 9 controls, 13, 14, 15, 16, 17, 18, 34 UI | TUI task strip/details/child views and actual execution events | Complete; verified |
| Plan review and permissions | 37 | Dedicated plan review, permission transitions, read-only enforcement and feedback | Complete; verified |
| Integration and end-to-end QA | All 37 | Root task: contract coordination, conflict resolution, full validation, real PTY checks | Complete; verified |

## Integration contracts

- One stable task identity must be published before work starts, with parent/child/job/terminal correlation, lifecycle timestamps and output access. UI must consume authoritative state rather than infer execution from transcript prose.
- Preserve durable session commit-before-event ordering and single-writer ownership. Use compatibility-safe event/schema changes with resume coverage.
- Share guards and explicit resolved configuration with native children; enforce authority at execution, not only schema exposure.
- Cancellation is cooperative, bounded and joined; show Cancelling until actual settlement. Preserve worktree artifacts before deleting execution directories.
- Monitor events use bounded queues, filters, deduplication/debounce and wake budgets. Retained output must remain readable after settlement.
- Plan acceptance carries an explicit target permission mode; rejected/dismissed/failed transitions remain read-only. Review includes the full detailed plan.
- Each workstream records its public API and commits only its implementation delta above the shared baseline. Root integrates dependency changes and owns cross-stream shipping composition.

## Internal task checklist

- [x] A01: P1 — Native first-turn cancellation is not propagated (Live + Source).
- [x] A02: P1 — `/stop` can wait without a timeout for that broken cancellation (Live + Source).
- [x] A03: P1 — Background PTY start and management defaults disagree (Live inventory + Source).
- [x] A04: P1 — Worktree editing results have no guaranteed handoff (Source).
- [x] A05: P2 — No visible child ID until the first turn ends (Live + Source).
- [x] A06: P2 — One-shot agents disappear from the live-child inventory (Source).
- [x] A07: P2 — Ordinary foreground subagent calls serialize (Source + Test).
- [x] A08: P2 — No general background option for arbitrary tool calls (Source).
- [x] A09: P2 — No foreground-to-background promotion control (Source).
- [x] A10: P2 — No shared concurrency/spend admission budget for the job tree (Source).
- [x] A11: P2 — Background runtime does not survive process shutdown (Source).
- [x] A12: P2 — Settled job history is process-local and not visibly bounded/managed (Source).
- [x] A13: P1 — No live child conversation switcher (Live + Source).
- [x] A14: P2 — No persistent bottom task strip (Live + Source).
- [x] A15: P2 — Task panels lack execution actions (Source).
- [x] A16: P2 — Child streaming events are suppressed rather than routed (Source).
- [x] A17: P2 — No per-child task telemetry view (Source).
- [x] A18: P2 — Concurrent tool display is not a live execution timeline (Source + Test).
- [x] A19: P2 — Background shell output is completion-only and truncated (Source).
- [x] A20: P2 — Background PTY completion does not include process output (Source).
- [x] A21: P1 — Monitor tool is missing (Source + live inventory).
- [x] A22: P2 — No first-class targeted watch configuration (Source).
- [x] A23: P2 — Custom agent schema is minimal (Source).
- [x] A24: P2 — Preset instructions are appended to the user prompt (Source).
- [x] A25: P3 — No custom-agent authoring/reload workflow (Source).
- [x] A26: P3 — No Claude/Codex agent-file import (Source).
- [x] A27: P2 — No fork-and-continue combination in the task interface (Source).
- [x] A28: P2 — Child follow-up blocks the calling tool turn (Source).
- [x] A29: P2 — No model-facing close/archive child control (Source).
- [x] A30: P1 — Default workflow is not an agent/tool workflow executor (Source).
- [x] A31: P2 — Workflow tool schema does not teach the definition shape (Source).
- [x] A32: P2 — Team creation does not launch a ready team (Source).
- [x] A33: P2 — Team mail is a durable mailbox, not automatic conversation delivery (Source).
- [x] A34: P2 — No team/task dependency UI (Source).
- [x] A35: P2 — External runtime capability parity is limited (Live catalog + Source).
- [x] A36: P2 — Parent configuration inheritance needs explicit semantics (Source, validation needed).
- [x] A37: P1 — Plan mode lacks the complete review-and-permission handoff (Source; user-requested gap).

## Validation gates

- Focused meaningful tests for lifecycle, policy, persistence and failure behavior per workstream.
- Workspace formatting, clippy, tests and generated-reference verification after integration.
- Real PTY scenarios for three running children, navigation, background output, stop, Monitor and all three plan decisions.
- Crash/resume and worktree tracked/untracked result preservation.
- Review actual provider request tool/config payloads and default composition.
- Close each audit ID with implementation links, test evidence and any honest platform/provider limits.

## Loop controls

Record a concrete failure and next hypothesis before retrying. Do not repeat unchanged tests after they pass. If a test is blocked by infrastructure, isolate the cause and continue independent work; do not mark its finding complete. Coordinate shared API changes before integrating dependent work.

## Provider independence and current feature requirement

User clarified twice on 2026-09-08: include current Claude Code/Codex coding-agent features, not an older snapshot; native features must work across configured models/providers, not depend on Claude subscriptions. Implement portable heycode-owned execution and orchestration. Verify model-capability-specific fallbacks explicitly and never count an unavailable hosted service or unsupported modality as working. Maintain a dated feature/provider matrix with primary-source references and executable evidence.

Requested tasks (all gpt-6-astra, high):

- Runtime: 01a07dad-83c4-7b71-a6df-210d2d3b94c2
- Custom agents: 01a07dad-83d2-7dc1-9aac-d23e7cf4213a
- Execution: 01a07dad-83c2-7012-8239-847477a8ef3f
- Orchestration: 01a07dad-83cb-72b0-ad90-cde43f33c280
- Task UI: 01a07dad-843d-75e0-8910-5b905f3f3cd2
- Plan: 01a07dad-8be0-7562-90f0-8100026aeaa0

Shared baseline commit: `174740a54b664ae2df2150ea2373f0718ea4201b`.

Additional current-feature tasks:

- Provider parity: 01a07db0-c3b8-7bf3-b77f-2bb6f1bbbc8d
- Current commands: 01a07db0-c3b8-7bf3-b77f-2bdfee8dadb9
- Remote/routines: 01a07db4-b9bc-7262-8ab9-41d7f51facfb
- Browser/desktop/voice: 01a07db4-b9d2-7e13-bb3e-121cc8dcd725
