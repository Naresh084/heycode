# Execution audit implementation

Workstream: findings 2, 3, 8, 9 runtime, 19, 20, 21, 22. Base snapshot: `174740a54b664ae2df2150ea2373f0718ea4201b`. Isolated branch `codex/audit-execution-20260908`.

## Current implementation

- Live byte-exact stdout/stderr via additive `ProcessOutputSink`, `OutputStream`, `SubprocessBackend::output_streaming` and `ShellBackend::execute_streaming`. Unsupported provider backends fail before launch; local processkit implementation retains ordinary containment/cancellation.
- PTY output observer is independent of destructive terminal reads. Execution plugin registers all management tools together when absent and uses the exact caller session owner. Job-to-terminal association remains after settlement.
- `ExecutionJobService::output(&JobId) -> Option<ExecutionOutput>`, `job_terminal`, `output_jobs`, `foreground_jobs`, `promote` are the UI contract. Pages have byte offsets, total/lost/retained metadata and terminal-safe lossy UTF-8 text.
- Private atomic retained-tail files persist settled output. Startup recovers an unfinished checkpoint as interrupted; live process survival is not claimed. Output retention and inline preview caps plus history count are exposed in `[tools]` and wired to execution plugin configuration.
- `job_output`, `monitor`, `run_tool` registered by default. Monitor watches one source job or launches a command; bounded line parsing, substring filters, duplicate suppression, debounce, rate limit, lifetime event cap and markers work independently of model/provider.
- General `run_tool` targets must attest `Tool::supports_background`; bash and MCP opt in. Target calls pass Agent approval and pre-tool guards. Foreground run_tool waits can be promoted under the same job ID.
- `/stop` immediately announces a cancellation request and bounds settlement waiting to five seconds, retaining truthful unconfirmed timeout feedback.

## Validation and remaining work

- `cargo check -p heycode-agent -p heycode-exec -p heycode-mcp` passed.
- Initial real-process suite: five tests passed; sixth corrected a test argument (`terminal_write` uses `input`) and then passed independently. Includes live stdout/stderr before exit, PTY input/output and identity, monitor filtering/deduplication before source exit, and foreground promotion.
- Retained output unit tests added for overflow, independent cursors, recovery metadata and private/symlink-safe persistence; running.
- The planned runtime, promotion, authority and bounded-monitor follow-ups are complete below.
- Optional WebSocket source is intentionally omitted; command and existing-output sources are the supported runtime contract.
- Neighboring changes: heycode-tools cancellation attestation, MCP adapter opt-in, heycode-config output caps, CLI factory wiring. Runtime owns JobRegistry; UI owns controls.

## Follow-up: runtime integration

- Integrated runtime prerequisite `5e80bdb` locally as `8236468`; this is not an execution delta to cherry-pick again.
- Ordinary model-issued bash calls now receive a job identity and stream output through the guarded tool pipeline. Promotion preserves the same running process and allows the parent turn to continue. `run_tool` also accepts cancellation-capable task orchestration without consuming a descendant process slot.
- Monitor uses at most eight coordinator slots, independent of process permits, plus `reserve_event_wake`. Output allocations use permits so concurrent admission cannot exceed the configured history bound.
- Added actual-process/caller tests for target guard denial, ordinary bash promotion after first live bytes, readiness without newline while eight source slots are occupied, retained output overflow and session resume, and immediate `/stop` acknowledgement followed by truthful unconfirmed timeout for an uncooperative worker.
- Focused `cargo test -p heycode-agent --test main execution_jobs`: **11 passed**, including process tests. Subsequent output unit tests and clippy remain to run against integrated dependencies.
- Identified and coordinating nested caller authority: shared tools must capture the caller's `ToolExecutionContext` before spawn and must retain caller session ownership, not the root service's Agent. Root provided code-mode task-local context; next commit addresses this before final completion.

## Follow-up: caller authority and lifecycle

- Captured `ToolExecutionContext` now carries exact caller tools, guard, approval, cwd, session, bus, cancellation owner and attachment store across task spawning. `execute_observed` and `execute_preapproved_observed` preserve this scope and live output. Rich results use the same media admission helper as ordinary tools.
- Shared execution wrappers resolve caller scope before starting or reading output. Retained scope records contain observations only; they do not retain child Session writers after close. Child shutdown cancels its owned shell/tool/monitor work. Terminal tools inherit a host-only task-local owner, so child PTYs can be managed without granting parent/sibling terminal access.
- `Agent::settle_job` delegates its unchanged reserve/enqueue/flush/commit/publish transaction to the captured context. Current feature integration should wrap its edit/question hooks at `ToolExecutionContext::execute_preapproved_observed`; `bus` and `session` are available there.
- UI additions: `write_terminal(&JobId, &[u8])` and `terminal_owner_for_job(&JobId)` select the actual child owner. Root `output`, `job_terminal`, `foreground_jobs` and `promote` can inspect child scopes for the human; model tools select their own scope first.
- `promote` now persists FollowUp delivery through runtime `set_delivery` before releasing the foreground waiter. Configured inline output caps apply to ordinary bash results. PTY launch supports cancellation and releases an uncommitted reservation when its opening future is withdrawn.
- Caller regression passed: inherited wrappers cannot dispatch parent-only tools or read parent output; child cwd, inbox and terminal input all remain correctly scoped. Output unit tests: **3 passed** (overflow/cursors, interrupted/completed recovery, private/symlink-safe persistence). Focused check passed after these changes.
- Locally integrated prerequisites only (not execution deltas): root `1cfa2de`, `314b512`, `7412038`; runtime `f49abfc`. Root already owns these originals.
- Worktree rebinding, owner-close and monitor-pressure follow-ups are complete below.


## Final execution delta

- Worktree rebinding covers `background_shell`, `background_terminal`, `monitor`
  command sources, and `terminal_open`. ScopedShellBackend now delegates live
  streaming and exposes its restricted subprocess authority for PTYs. Native
  terminal opening uses caller cwd and opening cancellation. An unsupported
  worktree PTY backend returns a bound refusal tool instead of falling back to
  the parent executor; a dedicated regression covers this.
- Actual macOS Seatbelt regression checks four launch paths: writes inside the
  child workspace succeed, writes to the parent directory fail. The fixture
  follows production's parent resolver plus rebound executor arrangement.
- Extended caller test proves owner shutdown settles its process as Cancelled
  and parent output observations do not retain the child's exclusive session
  writer. Real terminal input and caller inbox/output isolation remain covered.
- Monitor stress tests cover bounded parsing/batching for large UTF-8 lines
  and thousands of events, rate/lifetime limits, exhausted shared wake budget,
  no event after cancellation, and cancellation of an owned command while an
  existing source remains independent.
- Focused execution integration: **14 passed**. Execution unit filter: **5
  passed**. Exec terminal integration: **19 passed**; tool terminal integration:
  **4 passed**; shell contracts: **4 passed**.
- `cargo clippy -p heycode-exec -p heycode-tools --all-targets -- -D warnings` passed.
  Agent lint has no remaining execution-file diagnostics; it stops only on the
  existing nested `if` in `code_mode.rs:41`. The features stream's `fb923ad`
  replaces that method; root must run final agent/workspace lint after merging
  that delta. No unrelated lint edits are included here.
- Crate READMEs now document default tools, exact configuration bounds,
  caller/worktree authority, UI APIs, monitor controls, and the settled-tail
  versus crash limitation. Runtime `7d0f918` was integrated locally as
  `d6d8877` solely as a prerequisite; do not cherry-pick it as an execution delta.
- Root/UI retain responsibility for merged real-TUI journeys and workspace-wide
  checks. This workstream proves native process/PTY and provider-independent
  fake-provider integration, without claiming external-provider credentials or
  complete unbounded log durability.
