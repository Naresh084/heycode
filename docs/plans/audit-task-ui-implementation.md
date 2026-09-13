# Task UI implementation

Base: `174740a54b664ae2df2150ea2373f0718ea4201b`. Ownership: A09 controls, A13–A18, A34 UI.

## Result

The persistent bottom task strip reads actual root-owned child, tool, execution-job and team records. Counts distinguish running, waiting, cancelling and failed work. Ctrl+T (persisted `toggle-tasks` keymap action) or the strip opens a keyboard/mouse list. Selecting a task replaces only the visible transcript with its live/retained conversation or process output. The full parent editor, cursor, undo state and queued input retain their owner; each child has its own unsent editor. Esc returns through the list to the parent.

Details include actual lifecycle, owner/session/job correlation, runtime/model when reported, elapsed active-turn time, token usage, concurrent/current calls, committed changed paths and output retention. Alt+M opens scrollable metadata. Provider billing is explicitly unavailable when no billing event exists; elapsed time and usage are not invented. Alt+O selects stdout/stderr/terminal streams with independent byte cursors. PgUp/PgDn and Ctrl+End read retained pages or follow live output. Narrow terminals wrap action buttons across rows.

Enter steers the selected running native child at its next safe step or starts an idle follow-up on its existing handle. Alt+I requests interruption and waits for actual lifecycle settlement. Interrupted continuable children remain idle/resumable. Alt+X archives a settled conversation while retaining output. Alt+B promotes the exact eligible foreground execution job. Terminal input goes through `ExecutionJobService::write_terminal` so the service resolves the actual child owner. Failed control submissions restore unsent text into that child's draft; panics are contained by the owned operation future. Approval, trust, question and secret dialogs retain priority over task input.

Child slash commands use the selected native Agent and the composed registry for `/btw`, `/recap`, `/questions`, `/answer`, `/output-style`. Unsupported parent-only commands return a visible refusal and restore the draft. Team details project committed roster, per-task state/assignee/dependencies/readiness/results, and all mail with separate delivered/claimed flags. Workflow Saved/Node events have explicit transcript summaries.

## Ownership and boundedness

`TaskSource` is the single adapter boundary. `RegistryTaskSource` attaches native observers before child inference; listeners belong to the TUI source and are detached on drop. Committed Session events supply text/reasoning/final messages; final-message reconciliation replaces provisional chunks without duplication. Actual correlated `ToolExecutionEvent` admission/run/finish/commit phases make concurrent execution visible independently of ordered durable commits.

The adapter checks the actual root SubagentAuthority. Native observation retains at most 128 children (dead owners evicted first), 1,024 events/256 KiB per child, 32 KiB per event and 128 recent call records. Console output pages are bounded; source truncation/unavailability is visible. Team replay is cached by the last committed team revision. UI operations live in one bounded owned JoinSet and are cancelled/aborted on terminal teardown.

## Dependencies and integration

Own changes before the final UI commit: `c24657f` actual tool execution events, `0c86212` console/navigation, `5a612a1` live native observers.

Runtime dependencies: `5e80bdb`, `f49abfc`, `7d0f918` (including actual TaskSnapshot workspace metadata). Execution: `640e2a0`, `526a75c`, `1887bdd` with root's `7412038` current-agent context. Orchestration: `b917eee`, `24f22f7`. Local cherry-pick equivalents are dependency verification only, not additional UI deltas. Final adapter calls `write_terminal`, requiring `1887bdd`.

## Validation

- `cargo test -p heycode-tui task_console_ --quiet`: 14/14 passed against runtime `f49abfc`, execution `526a75c`, and orchestration `24f22f7` before the final `write_terminal` adapter substitution. Covers authoritative counts, screen-reader parity, mouse hit regions, exact parent/child drafts, retained paging, cancellation lifecycle, source failure, three simultaneous native children, actual terminal input/output, promotion with exactly one command invocation, separate output streams, failed draft restoration, custom keymap, narrow control wrapping/metadata scroll, and committed team dependency/claimed-mail projection.
- Actual CLI/native runtime PTY journey passed through a localhost HTTP provider fixture: three simultaneous streaming children; live text and reasoning; mouse switching; draft preservation; queued steering; retained output after close; cancellation while the provider remains held; real terminal stdin/output; foreground promotion with exactly one invocation. Raw ANSI and reconstructed actual screens: `/tmp/dshx-task-ui-pty`.
- Reproducible harness: `PYTHONDONTWRITEBYTECODE=1 <python-with-pyte> scripts/task_console_pty.py --binary target/debug/heycode --out /tmp/dshx-task-ui-pty`. Uses disposable home/workspace and no paid provider calls. The provider fixture is an HTTP transport fixture, not a replacement task runtime.
- Final integrated verification completed in an isolated checkout of coordinating snapshot `e8f7c70` plus `e1ce65c` and the final small follow-up: 15/15 focused task-console tests passed (including approval preemption with both drafts preserved); `cargo clippy -p heycode-tui -p heycode-ui --all-targets --no-deps -- -D warnings` passed; real CLI build passed; complete native HTTP/PTY journey passed again. The final journey adds a real resize to 40 columns with metadata paging and visible wrapped controls, live `PROMOTION_READY` before detachment and retained `PROMOTION_DONE` after completion, with exactly one invocation. Evidence: `/tmp/dshx-task-ui-integrated-pty/result.json` plus 12 reconstructed actual screen/ANSI captures. The integration snapshot's unrelated `session_control` dead-code warnings remained outside TUI/UI lint scope. No claim of live-account validation.

## A35 follow-up: actual provider readiness in `/agents`

Added after coordinating snapshot `2132ff5`. The actual `/agents` command now
opens an owned readiness run using `SubagentRegistry::provider_readiness` for
exact provider IDs. At most 32 providers are checked, with four concurrent
futures and a two-second deadline per provider. Cancellation propagates to each
provider token; close, replacement, and teardown abort the owned work. `c`
cancels in place with visible Unknown/cancelled rows; `r` starts a fresh probe
set. Late results from an old panel cannot populate a new one. Returned errors
and panics are rendered as Unknown with safe fixed reasons, not raw provider
error text. The operation performs no start or authentication request.

The catalog and screen-reader output distinguish Ready, NeedsAuthentication,
Unavailable and Unknown from advertised capability evidence. The selected
provider has wrapped capability/setup detail; long inventories scroll selection
into view. NeedsAuthentication points to the provider's setup without opening
it. The existing single-panel claim boundary protects child/parent editor and
modal ownership while opening the catalog.

Validation: 3 focused readiness tests cover emitted `/agents` command output for
missing-authentication, ready, unavailable and default unsupported-probe states;
capability evidence in real rendered cells; 40 registered providers proving the
four-worker/32-probe bounds; visible cancellation and token propagation;
close/reopen freshness; and a real two-second timeout that stays Unknown. Full
panel suite and real CLI evidence are recorded after final verification below.

Final A35 verification passed: all 21 panel-command tests; strict TUI clippy
(`--all-targets --no-deps -- -D warnings`); real CLI build; and the complete
native HTTP/PTY journey, now including `/agents` showing the native provider's
actual Ready response with fork/continuation/interrupt evidence and an explicit
recheck. Evidence: `/tmp/dshx-a35-readiness-pty/result.json` and
`agents-native-readiness.txt`/`.ansi`, alongside the 12 existing task-journey
captures. Missing credentials, unknown support and cancellation are registry
probe fixtures driven through the real command/catalog/render boundary; the
real CLI capture verifies the production native readiness probe. No external
login or child launch is performed by opening the catalog.
