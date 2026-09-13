# Phase 2 `/exit` completion audit

Date: 2026-09-11 (Australia/Melbourne)

## Verdict

`P2-C-exit` is **complete for the accepted local safety contract**. `/exit` is
the compatibility alias of canonical `/quit`; both resolve to the same
`Interrupting` command. Idle submission exits normally. During active native
work, dshx adds a deliberate cancel-default confirmation before it interrupts
the active owner, waits for settlement, and runs the queued quit. That safety
step is an intentional presentation difference from the current Claude source,
which exits immediately when `/exit` is submitted during a live response.

The command neither becomes model input nor implicitly stops detached owners.
Provider/session-wide termination remains owned by the normal application
shutdown path.

## Actual terminal evidence

The final active-turn result is
`exit-active-paired-20260911T122117Z-e07db6/result.json` (local-only evidence: `tmp/terminal-evidence/exit-active-paired-20260911T122117Z-e07db6/result.json`).
It used two disposable homes/workspaces, deterministic streaming servers bound
to `127.0.0.1`, dummy credentials, and actual PTYs. It made three loopback
provider requests and zero external or commercial provider requests.

- dshx immutable binary SHA256
  `94633f465265a7869daa12dd1de35fd7b070344b51ab870bbef1fcf19c1f5530`
  displayed a live partial response, then a cancel-default `Interrupt active
  work?` dialog. Escape restored the exact `/exit` draft without interrupting
  or issuing another request. Selecting `Interrupt & run` emitted the queued
  lifecycle receipt, aborted the turn, and exited with status 0 without waiting
  for the fixture to release its response.
- The retained native journal contains one `user/message`, one `turn/start`,
  one `request/header`, and one `turn/end` with reason `aborted`. `/exit` occurs
  in neither the journal nor the provider request.
- Claude Code 2.1.268, immutable SHA256
  `06a96d5423f83770f120859f1c58e60d7252cc4c122aa13043b7e7cd716bc76a`,
  ran the Opus 5 route against the localhost Anthropic fixture. Typing `/exit`
  kept the process alive and showed the exact local command; Enter exited with
  status 0 in the active state without a prior Escape or process signal. The
  source issued two loopback message requests during that lifecycle; neither
  contained `/exit`. One refused auxiliary `downloads.claude.ai` proxy attempt
  was recorded, and no external request completed.

The independent idle comparison is
`exit-paired-20260911T121233Z-8896db/result.json` (local-only evidence: `tmp/terminal-evidence/exit-paired-20260911T121233Z-8896db/result.json`).
Both actual CLIs kept running while `/exit` was only a draft and exited 0 only
after Enter. The native idle journal contains only `session/created` and
`runtime/linked`; no prompt or request event was created. Claude's captured
command menu identifies `/exit` as `Exit the CLI`.

Applicable presentation states are covered: idle, exact draft, live response,
cancel-default confirmation, Escape cancellation with draft restoration,
explicit interrupt selection, queued/running receipt, aborted turn, and clean
process exit. Generic failed, grouped, expanded, and long-output command-card
states do not apply to this process-lifecycle command. The confirmation exposes
keyboard choices only; no pointer action is advertised in the accepted local
contract.

## Source and regression boundary

- `crates/heycode-agent/src/commands.rs` registers canonical `/quit` with
  `CommandTiming::Interrupting`.
- `crates/heycode-agent/src/command_descriptor.rs` maps `/exit` to `/quit` in the
  central alias table.
- `crates/heycode-tui/tests/it/command_scheduling.rs` covers cancel-default
  selection, exact draft restoration, interrupt dispatch, settlement gating,
  and single promotion of the queued command.
- `crates/heycode-tui/src/app.rs` bounds cancellation/join and has a regression for
  a task that ignores cancellation.

The immutable v8 TUI build used by the active PTY follows a full passing run in
`tmp/shared-full-tui-20260911T115404Z-a794f8.log`: 137 unit tests (2 existing ignored), 11
additional integration tests, 349 main integration tests, and 7 standalone
tests passed. A fresh focused recompile after the PTY run did not execute
because the shared working tree currently has an incomplete, separately owned
`HumanCommandRequest::OpenAdvisor` addition with no matching `app.rs` arm. The
exact compiler evidence is retained in
`tmp/terminal-evidence/exit-command-confirmation-test.log` and
`tmp/terminal-evidence/exit-bounded-shutdown-test.log`; it is not an `/exit`
failure, but the coordinator must settle that shared edit before a whole-tree
green claim.

