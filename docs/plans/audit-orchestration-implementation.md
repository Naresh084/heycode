# Orchestration audit implementation

Workstream: backend findings A30, A31, A32, A33 and A34. Branch
`codex/audit-orchestration-20260908`, based exactly on shared snapshot
`174740a54b664ae2df2150ea2373f0718ea4201b`. The worktree was clean before branch
creation. Only this worktree was edited. No additional agents or tasks were spawned.

## Implemented behavior

- **A30:** Default CLI workflow factory mounts the native graph worker. Version
  2 has native one-shot agent actions, guarded plain-JSON tools, recursive result
  bindings, fan-out/fan-in, explicit acyclic dependencies, boolean conditions,
  skipped-dependency propagation, bounded retries/backoff, 1–8 concurrent nodes,
  and durable per-node intent/result. Version-one deterministic definitions and
  the replaceable worker API remain supported. Native agent steps explicitly
  select the native provider; their inference route can use any configured model
  provider. Unknown crash-left effects are refused, completed effects never
  replay, and forks cannot resume ancestor runs.
- **A31:** Complete nested model schema, limits, capability names, binding syntax,
  repairable serde/validation errors and a runnable definition example. Added
  cooperative safe-boundary pause/resume and save/saved/run_saved controls for a
  bounded session-local reusable definition library.
- **A32:** Default team creation launches native worker/reviewer roles, with
  custom role instructions and explicit empty-roster compatibility. Newly ready
  roles enter one atomic roster event after every startup succeeds; startup
  failure closes all newly created retained children. Task creation dispatches
  ready Pending work by default; completion releases dependent work and supplies
  dependency results. Existing JobRegistry remains the sole background owner;
  durable state enforces one running task per assignee. Internal settlement CAS
  retries prevent lost parallel completions. Explicit interrupted recovery skips
  tasks still owned by live jobs; Blocked work is not automatically replayed.
- **A33:** Owner-aware durable inbox admission, SHA-256 insert-once mail identity,
  source attribution, delivered/claimed replay state, automatic native inbox
  turns through JobRegistry and the child gate. Claim means explicit recipient
  acknowledgement, separately from durable delivery. Unsupported/unavailable
  handles report a delivery error while retaining mail. Recipient model snapshots
  cannot read mail between other members. A generation counter avoids losing
  mail arriving at a conversation job's settlement boundary.
- **A34 backend:** Existing typed projections plus public JSON renderer expose
  member activity, tasks, dependencies, runnable state, result summaries and
  durable mail delivery/claim rows. Frontend task board belongs to the UI workstream.

## Public/API integration contracts

- `Agent::execute_workflow_tool(name: String, args: Value, token: CancellationToken)
  -> anyhow::Result<Value>` is `pub(crate)` and uses `admit_call` plus
  `execute_call`; the workflow journal owns durable intent/result. Isolated
  initial commit: **b917eee**. Root is generalizing this entry to task-local actual
  Agent context for Code Mode; preserve that integration over this initial seam.
- `native_workflow_plugin()`, `NativeWorkflowWorker::new(agent, subagents,
  authority)`, `workflow_definition_schema()`. The native plugin injects the
  subagent registry in addition to existing workflow services.
- `WorkflowWorker::supports_graph()` defaults false; native worker returns true.
  `WorkflowWorkerRequest::pause_requested()` and `WorkflowWorkerResult::paused()`
  let replacement workers honor safe pause boundaries.
- `WorkflowService::{pause,save,run_saved}`; `WorkflowDefinition::{graph,is_graph,
  max_parallel}`; `WorkflowStep::{with_graph_policy,depends_on,when,max_attempts,replay_safe}`;
  `WorkflowRunProjection::nodes`; `WorkflowProjection::definitions`.
- New `WorkflowCapability::{Tool,Agent}`, `WorkflowChange::{Node,Saved}`,
  `WorkflowNodeRecord`, `WorkflowNodeState`, and `WorkflowOutcome/State::Paused`.
  Node rows must refer to an immutable declared graph node. Root's dynamic
  JavaScript engine uses a separate explicit Code Mode domain rather than
  weakening graph replay to admit arbitrary undeclared nodes.
- `TeamBootstrapRole`, `TeamService::{bootstrap,dispatch_ready,deliver_pending}`,
  `TeamView::mail()->Vec<(&TeamMailboxMessage,bool delivered,bool claimed)>`,
  public `render_team(&TeamView)->Value`.
- `SubagentHandle::deliver_mail(id,text)->Result<bool,SubagentError>` defaults
  Unsupported; native override is idempotent durable admission.
  `SubagentHandle::run_mail(token)->Result<(),SubagentError>` defaults Unsupported;
  native override consumes existing queued inputs. Runtime's identity-alias
  wrapper must forward both methods (coordinated with runtime task).
- New session `TeamChange::{MembersBootstrapped,MessageDelivered}` and
  `InboxSource::Team`. These extend existing session event payloads; no new
  `SessionEventKind` tag was introduced.
- Neighboring wiring changes: agent guard entry, native handle/inbox helper,
  exports, CLI default workflow factory, and `sha2` dependency for stable mail
  identity. Root owns overlaps with runtime/Code Mode changes in those files.

## Validation

- `cargo test -p heycode-agent --test main`: **217 passed**, including the native
  orchestration fixture suite and existing workflows, teams, subagents, inbox,
  guards, jobs, runtime adapters and turn lifecycle tests.
- `cargo test -p heycode-agent --test main it::orchestration`: **16 passed** on the final
  source, including ownership reservation, pause validation and serialization
  compatibility changes.
- `cargo test -p heycode-session --test main`: **140 passed**, including new strict
  graph replay, unsafe-pause refusal, atomic roster/delivery/claim reopen,
  foreign-delivery refusal and legacy serialized field compatibility tests.
- `cargo clippy -p heycode-agent -p heycode-session --all-targets -- -D warnings`:
  **passed** on the final source.
- `cargo fmt -p heycode-agent -p heycode-session -p heycode-cli -- --check` and
  `git diff --check`: **passed**.

The 16 native orchestration tests cover a real two-tool barrier (parallelism,
not elapsed-time guessing), native child tool invocation and downstream real
file write, live permission denial before effects, child workflow owner
isolation, dependency result bindings, conditional branches and skip propagation,
bounded successful/failed retries, joined cancellation, safe pause/save/resume
without repeating the effect, crash-left unknown intent refusal, ancestor-fork
resume refusal, default native role bootstrap, atomic startup failure cleanup,
automatic dispatch with concurrent fan-in, recipient-specific durable mail and
single insertion, live-task recovery refusal, and empty-response settlement.

Testing exposed and fixed empty child-response settlement and an admission /
recovery reservation gap. The bootstrap failure test uses an explicit provider
protocol error; exhausted FakeProvider scripts are deliberately not treated as
failure evidence, because that fixture returns successful empty responses.

## Integration dependencies and honest limits

- Native first-turn cancellation and child lifecycle/history/alias wrappers are
  owned by the runtime workstream. This work uses the existing APIs and tests
  follow-up and tool cancellation, without claiming that the snapshot's known
  first-turn defect was fixed here.
- Native workflow model control routes each caller to its actual owning Agent
  and session. Admission captures `ToolExecutionContext`; tool steps retain that
  registry, guard, approval, cwd and cancellation owner. Agent steps scope both
  `TASK_AGENT` and `TASK_AUTHORITY`, retaining actual native parent lineage.
  Workflow jobs use coordinator admission while descendant providers obtain
  their own shared execution permits. Owner shutdown cancels and joins work.
- Root has implemented one JavaScript engine and Code Mode service. Graph v2 is
  not claimed to implement arbitrary script workflows; root wires script calls
  through the shared guarded native dispatcher after merging.
- `WorkflowTool::untrusted_content` uses
  `UntrustedContentBoundary::tool_orchestration()` for status and node results.
- Pause is cooperative at committed batch boundaries; a long tool must settle
  before Paused appears. Cancellation, service shutdown and actual job/process
  survival remain separate lifecycle concepts. These are process-owned jobs,
  not a detached service surviving app shutdown.
- Automatic mail is a queued native follow-up; it does not interrupt an active
  provider/tool turn. Explicit recipient acknowledgement remains separate from
  delivery. Unsupported external adapters never fabricate native mailbox parity.
- Saved definitions, run state and status are projected from the owning session's
  local event suffix. Forks cannot inspect or invoke inherited ancestor definitions
  or runs. This is a session-local library, without an arbitrary file loader.
- Full CLI composition, full workspace QA, frontend integration, provider matrix,
  real PTY journeys and external runtime readiness are root integration gates.


## Caller ownership follow-up

Branch `codex/audit-orchestration-owner-20260908` starts at integrated snapshot
`e8f7c70`. It replaces blanket child refusal with actual caller execution and
settlement. Native owner lookup validates the captured session writer against
`SubagentRegistry::agent_for_authority`, including aliased task identities.
A bounded weak service cache avoids retaining idle child Agents/session writers;
active jobs retain their service and dispose through the existing JobRegistry.
Completion notices commit to the captured child inbox before job settlement.

Two crate-internal regression tests cover an actual native child's restricted
registry and cwd, denied parent definitions/runs/status, successful permitted
workflow and native grandchild ownership, child-only journal/inbox settlement,
captured approval denial, joined owner shutdown, and refusal after shutdown.
The integration child test now asserts successful nested execution. Fork tests
also assert that inherited run status and saved definitions are hidden. Team
fixtures use registry task IDs and actual native child sessions instead of
provider-local IDs or guessed session directories.

Follow-up validation:

- `cargo test -p heycode-agent --lib workflow::ownership_tests`: **2 passed**.
- `cargo test -p heycode-agent --test main it::orchestration`: **16 passed**.
- `cargo test -p heycode-agent --test main it::teams`: **3 passed**.
- `cargo test -p heycode-agent --test main it::workflows`: **3 passed** on the final
  delta with temporary prerequisites removed.

The e8f7c70 baseline required two temporary integration prerequisites for these
checks: the missing `false` argument in the existing compaction summary unit
fixture, and the `AliasedHandle` mail forwarding block from runtime `2ddb5bd`.
Those changes are excluded from this delta; root owns them. Without runtime
forwarding, the baseline mail-delivery test fails while the other 15 orchestration
tests pass. Existing `session_control` dead-code warnings are also baseline.

Default `cargo clippy -p heycode-agent --all-targets` completed with no diagnostics
in this delta. Strict `-D warnings` is blocked by baseline warnings in Code Mode,
execution foreground/jobs/output, and current features; default clippy also
reports an existing test lock held across await in `tests/it/execution_jobs.rs`.
Changed Rust files pass targeted rustfmt checking, and `git diff --check` passes.
