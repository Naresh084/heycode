# Runtime workstream implementation

Base: `174740a54b664ae2df2150ea2373f0718ea4201b`. Branch `codex/audit-runtime-20260908`.
Owns A01,A04,A05,A06,A07,A10,A11,A12,A27,A28,A29,A35,A36.

## Public API coordination (in progress)

- Existing `SubagentRegistry` remains the child owner. Adding `task_snapshots_for(authority)` and `task_snapshots()` for authoritative lifecycle inventory; task IDs allocated before provider inference. Snapshot includes owner, provider, session/job correlation, status, output, revision and timing.
- Adding early `native_child_for(authority, id)` access to the actual `Arc<Agent>` for child UI/transcript/events and inbox steering. Native children retain their own EventBus (subscribe on child Agent.ui()).
- `start_background` remains JobId-returning for compatibility; new admission method returns both task ID and job ID. Existing child handle APIs remain.
- Jobs admission/history changes will touch `jobs.rs`; execution workstream should preserve these additions when integrating its output/cancelling APIs. Tool scheduling receives explicit orchestration effect, never a false ReadOnly designation.
- Worktree manager cleanup will preserve tracked, untracked and committed divergence before removal, including recovery after interrupted process.

Validation and final integration notes will follow implementation.

## Implemented API / validation checkpoint

Runtime commits `5e80bdb`, `f49abfc`; custom config-only dependency is `3e5654b` (cherry-picked here as `2a338a4`). Further native worktree/scoped-service commit follows.

- `TaskSnapshot` fields described above are implemented; additive `workspace: Option<String>` preserves the result location separately from truncated text. Native observer `attach_native_child_observer(NativeChildObserver) -> NativeChildObservation` runs after session/Agent publication and BEFORE first send. Guard drop unregisters observer. UI derives usage/current tool from authoritative child session and event bus.
- `JobLimits`, `agent_plugin_with_job_limits`, `SubagentBudgetLimits`, `subagent_plugin_with_budget` are shipping configuration paths. `[jobs]` keys: `max_concurrent=8,max_queued=64,max_history=256,max_admissions=4096`. `[subagent]`: `max_concurrent=8,max_provider_requests=256,max_output_tokens=8192`. Limits are validated by CLI. Provider request reservations persist and never replenish on automatic turns. These are dispatch/output spend guardrails, not a promised dollar-denominated billing ceiling; provider transport retries/compaction retain their own bounds.
- Job state adds `Queued`, `Cancelling`; outcome adds `Interrupted`. `set_delivery(&JobId,InboxDelivery)->bool` is atomic with persistence before settlement reservation, for foreground promotion. `reserve_event_wake()->bool` shares completion wake budget with Monitor. `spawn_coordinator` avoids holding a process slot while waiting on descendant inference.
- Universal Agent composition attaches bounded jobs.json. Subagent bridge attaches bounded per-task JSON history and durable inference counter. Reopened live/retained tasks are Interrupted, never falsely Running or silently replayed.
- `Session::fork_with_metadata` preserves verified prefix lineage while writing actual child cwd/runtime (source must be Fork).
- Native child methods now `run_child(&SubagentRequest,CancellationToken)` and `run_child_at(&SubagentRequest,CancellationToken,cwd)`. Outer owns worktree lifecycle; inner builds the actual native Agent. Custom workstream should insert config/instructions/memory in inner after resolved inherited tools and route, using original parent cwd for memory scope.
- Native Worktree captures tracked patch and bounded untracked files without modifying parent index; continuable lease stays alive across follow-ups; one-shot and close preserve results. Git manager preserves tracked/untracked/ignored/committed divergence on success/cancel/failure/recovery; inspection failure retains worktree. Actual child workspace is recorded durably.
- Sandbox scope stays inherited: `SandboxService::for_workspace`, `SubprocessService::local_with_sandbox`, `ShellService::with_executor`; `Tool::rebind_workspace` and `ToolRegistry::for_workspace` rebind core read/write/edit/glob/grep/bash while keeping settings. Fixed-argv host Git management is separate from model command confinement.

Verified so far: native provider registry unit tests 6/6; job unit tests 8/8 including saturation/cancel/restart/panic; integration subagent/worktree batch 29/31 (two obsolete identity/state assertions repaired, identity regression rerun passed); native worktree end-to-end 2/2 including real macOS WorkspaceWrite filesystem and shell sandbox, follow-up and retained close. Clippy agent/tools/exec all-targets passed before final nested-background parent-capture adjustment. Additional regression checks remain before final handoff.

Root integration must combine the config schema version bump with other workstreams; preserve custom request config/instructions/preset identity and apply explicit config AFTER inherited route. Orchestration's `SubagentHandle::deliver_mail` and `run_mail` need forwarding through the runtime `AliasedHandle` wrapper. Execution is implementing child cwd/session/owner routing and rebind_workspace for background shell/terminal/monitor. UI owns child event buffering and real tool execution events.


## Final runtime delta and acceptance evidence

After `7d0f918`, runtime delta `2ddb5bd` adds explicit mail forwarding,
`scoped_agent()`, legacy job-admission persistence, unsupported-account-query
fallback to Unknown readiness, foreground parallel/fork continuation tests,
and persistent shared-budget coverage. The native exact NextTurn inbox API (originally added for remote use) is
`62ed327` (local cherry-pick `56f23c4`); the final runtime delta builds on it. The HTTP remote host was later removed
at the user's request; native inbox delivery and the stdio API remain.

The final delta adds `queue_message`/`SubagentMessageReceipt`, native exact
inbox turns for either queue, an idle-race driver, queued-message cancellation,
and true non-interrupting native steer. It reserves a job before durably
appending native input. Existing `send_background` remains JobId-compatible.
A native child not yet published has a job receipt until its session exists;
external follow-ups use the same owned job path and external steer is refused
unless the adapter provides an explicit contract. Model results include both
job and inbox identities when available.

True close now serializes with all follow-ups and permanently refuses stale
handles. Native mailbox turns and exact pending-input turns scope both Agent
and authority. Native nested background completions automatically resume an
idle continuable parent using an owned bounded coordinator after an admitted
wake; its own settlement is silent to prevent recursive notices. Root host
conversation wake behavior remains unchanged.

Acceptance verified locally: 30 subagent integration tests passed together,
then the added UI-independent nested-native wake test passed; all 74 library
tests passed; all 3 Git worktree integration tests passed. These include native
first-stream/first-tool cancellation, three early visible background children,
three ordinary foreground children at once with ordered results, parent model
switch inheritance, fork-plus-continuation, archive/restore, busy and idle steer
with exact-once durable input, queued message cancellation, close during an
active follow-up, and actual macOS worktree file/bash confinement. Final full
integration and strict clippy results are reported with the handoff commit.

Boundaries for root closure: restart reports Interrupted and never claims that
process-owned work survives; archive/restore retains a settled continuable
child only in the current process, and does not reconstruct finished one-shot
or prior-process handles. Durable histories retain summaries and session ids.
Shared dispatch/output guardrails are not a dollar billing ceiling. Installed
external provider canaries remain opt-in and were not run against paid accounts.
Readiness is available through `provider_readiness`; the UI/catalog workstream
must consume that method to display current readiness alongside static modes.

Integration: keep orchestration `deliver_mail`, use the final native `run_mail`
with both task scopes, and keep the AliasedHandle forwarding from `2ddb5bd`.
Keep execution child caller context/job settlement and all background-tool
workspace rebindings. Keep custom instructions/configuration/preset identity
and memory binding; use the native worktree outer lease path instead of the
custom branch's temporary worktree refusal. Root owns the combined config
schema bump and final CLI/UI acceptance.

Final budget correction: zero `max_provider_requests` or `max_admissions`
means no admission and is accepted by the CLI. The public constructors preserve
zero lifetime counts. CLI validation rejects out-of-range concurrency (1..256),
queue (0..4096), history (1..4096), and output tokens (1..131072). Direct library
constructors clamp concurrency to 1..256, queue to at most 4096, history to
1..4096 and output tokens to 1..131072; effective subagent values are visible in
the budget snapshot. Implicit output caps are bounded by selected model metadata
in native adapter/audio routes (and cached metadata for legacy routes). Explicit
requests remain unchanged for ordinary model validation; requests beyond the
shared output guardrail are refused. Zero-admission tests prove no job or
provider request executes; model-cap boundary tests pass.

The complete local agent integration run passed 206/213. Remaining failures are
for root integration: three review fixtures expect removal/exact output despite
conservative worktree preservation; three team fixtures use old backend IDs
instead of stable public task IDs; one workflow restart fixture retains a session
writer at reopen. All runtime-focused cases in that run passed. The root owns
those neighboring updates and the strict custom-adapter regression after the
model-aware output-cap fix. Do not report the whole integration suite as green
from this branch alone.
