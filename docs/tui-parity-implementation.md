# TUI parity implementation tracker

Scope: implement all gaps from `docs/tui-parity.md` in the current heycode checkout. Preserve existing work, native architecture, and configured model. No right-side diff view or large boxed side pane. User will perform a final retest.

- [x] T1 — Background execution: removed automatic deadlines from background shell/terminal jobs. Real 120-second ticker completed all 12 ticks.
- [x] T2 — Execution metadata: retain command, cwd, start/end, elapsed, exit/failure reason and expose appropriate shell details.
- [x] T3 — Task state: correlate wrapper/child jobs; consistent inventory and counts; child approval owner and waiting state.
- [x] T4 — Navigation: compact main/child radio selector; independent focus/selection; Down/Up/Enter/mouse; preserve drafts, scroll, and correct message owner.
- [x] T5 — Shared conversation presentation: semantic child messages and compact tool groups, expandable raw detail; avoid diagnostic clutter in the normal flow.
- [x] T6 — Background inspector: lightweight bottom list/detail within the same column; live output and applicable controls.
- [x] T7 — Rendering polish: correct multiword inline code, wrapping/grouping/spacing, item-scoped notices, clear partial/blocked outcome cues.
- [x] T8 — Verification: meaningful regression tests, format/lint/build, deterministic screenshots, live two-child and background-job replay, rebuilt heycode ready for user retest.

Implementation and verification are complete. The six-package run passed 1,414 tests; the final TUI run passed 371 tests (67 unit and 304 integration), adding one test after that run, for 1,415 passing tests across the verified packages. The final child cancellation test also passed after extending it to cover automatic queued-work continuation and repeated cancellation. Three optional tests remain ignored in the ordinary suite; the 120-second execution test was run separately and passed.

The CLI was rebuilt from the current checkout. Targeted Clippy is clean, documentation verification passes, and `git diff --check` passes. The real OpenRouter replay kept `meta/muse-spark-1.3`, opened three read-only native children, and completed all twelve ticker outputs in 120.1 seconds with no deadline. The voice-model Git status was unchanged. A separate deterministic native-runtime PTY replay passed all 12 captured screens, including multi-agent spawn grouping, queue recall, interruption, draft restoration, narrow layout, and terminal input/completion. Screens and replay result are linked from [the updated audit](tui-parity.md).

The live provider ticker screenshot predates only the final presentation cleanup (idle spinner removal and adjacent-spawn grouping). The final deterministic PTY screenshots exercise those changes in the rebuilt CLI. They are controlled-provider evidence, not claims about external model speed or reliability.

## Additional controls and queue acceptance criteria

- [x] T9 — Esc/Ctrl+C interrupt active agent and tool execution, including child conversations. If queued input exists, settle cancellation and submit the next queued work; another cancellation stops that run. Keep modal ownership explicit.
- [x] T10 — Show multiple pending messages below the working indicator. Transfer each consumed message exactly once into the transcript as a user message, preserving order and owner.
- [x] T11 — Up recalls all pending messages into the composer, removes them from the pending queue atomically, and puts the cursor at the end for further editing.
- [x] T12 — Regression coverage for repeated cancellation, active tool cancellation, queue consumption/recall races, multiple messages, and main/child routing. Quality takes priority over speed.

User correction: background shell and terminal execution must have no automatic time limit. T1 now removes the background deadline rather than adding a bounded default or deadline override.

- [x] T13 — Support multiple simultaneous subagents without an arbitrary configured count limit; verify creation, switching, queued input, and independent cancellation with several children.
- [x] T14 — Render compact child-spawn trees in the main transcript, using the inspected Claude UI as the reference: meaningful child labels, activity/status, clean indentation and spacing, and navigation into each conversation.


## Controls for retesting

- `Ctrl+T` opens the conversation selector. Arrow keys move focus; Enter selects. An empty composer’s Down key focuses the task strip first.
- `Alt+Left` returns to the parent. Each conversation retains its own composer and output position.
- Esc or Ctrl+C interrupts the active conversation, including its running tools. Queued work can start after settlement; another interruption stops that new turn. Approval dialogs target their displayed owner.
- Enter queues additional messages while busy. Up moves all still-pending messages into the composer, with the cursor at the end. Claimed messages remain in the transcript exactly once.
- Background jobs open in a compact bottom inspector. `Alt+M` shows full details, `Alt+O` changes output stream, and `Alt+F` expands output.
- Adjacent agent spawns share a compact tree; click a child to open it. Successful tool groups expand with click or keyboard focus and Enter. `Alt+R` exposes raw child events.

No commit, push, or worktree was created. Native OS mouse/clipboard behavior, external screen-reader software, and exhaustive long-session stress remain outside this verification, as in the original audit.
