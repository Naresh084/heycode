# Native workflow progress backend

Base: `92ffd2a`. Branch: `codex/workflow-progress-backend-20260908`.
This delta supplies authoritative data for the workflow workspace. It does not
edit the TUI or reexecute work to populate a display.

## Definition metadata

`WorkflowDefinition` accepts optional `title` and `phases: [{id,title}, ...]`.
`WorkflowStep` accepts optional `phase_id`. Phases retain their exact declared
order. When phases are supplied every step must reference one, phase IDs must be
unique, and every declared phase must contain a step. Without phases each step
forms a phase using its existing ID and label. Without title the exact definition
name remains the title. Metadata does not add or change graph dependencies.
Version 1 and 2 remain supported and old definitions retain their serialized
field set. [Runnable example](../examples/workflow-sequence.json) has three phases
and three parallel native agents in its middle phase.

## UI contract

Exported from `heycode-agent`:

- `WorkflowView`: run ID, explicit title/purpose, state, attempt, ordered phases,
  counts, timing, measured usage, current safe controls and live job ID.
- `WorkflowPhaseView`: declared ID/title, ordered nodes, actual linked agents,
  aggregate state/counts/timing/usage.
- `WorkflowNodeView`: exact ID/label/dependencies/kind, current state/attempt,
  committed result, original prompt JSON or binding, measured timing.
- `WorkflowAgentView`: stable public task ID, actual native session ID, optional
  job, exact node/attempt, label, original resolved prompt, bounded committed
  summary, lifecycle, actual timestamps, measured usage and timing.
- `WorkflowViewState`, `WorkflowNodeKind`, `WorkflowCounts`, `WorkflowTiming`,
  `WorkflowUsage`, `WorkflowControls`.

`WorkflowService::{views,view}` read the service's actual owner session.
`views_for`, `view_for`, `pause_for`, `resume_for` and `cancel_for` additionally
validate the full native `SubagentAuthority`, including its registry token.
`pause` requests a safe boundary; `resume` keeps durable completed effects;
`cancel` cancels and joins through the owned job. Stopping paused or interrupted
history appends cancellation without admitting or resuming work. Pure
`project_workflow_views(session, now_ms)` grants no controls and treats unfinished
history without a live service as interrupted.

Completion counts use `(completed + skipped) / total`. Failed/cancelled counts
are separate. Non-agent steps remain ordinary nodes. A failed future dependency
never invents a native agent or a successful node. Previously completed phases
remain completed when later work fails or is cancelled. `Closed` records do not
prove success: use the correlated node outcome as well as the actual task state.

## Correlation and retention

`WorkflowChange::Agent` records `WorkflowAgentRecord` inside the existing workflow
journal domain. A host-only `TaskObserver` is attached to the native request at
workflow admission. It records the public task ID before provider startup, then
the real session ID during `TaskRecord::publish_native`, before `child.send`.
Subsequent task revisions, measured response usage, and terminal summaries are
flushed into the owning workflow journal. Correlation cannot be rebound to a
different run/node/attempt/owner/session; regressions and duplicate node-attempt
links are refused. Retries produce distinct actual tasks under their exact node
attempt. Direct graph agents are correlated; their own further delegated tasks
remain accessible through the actual task conversation.

The observer retains only a weak owner session reference. Task revision updates
serialize publication while holding the task mutex, then acquire only the owner
session for the journal append. The view builder never reads a task registry or
task mutex while holding a session. Admission publication failure prevents first
inference. History is readable after both Agent handles and the registry have
been disposed, and the registry does not retain an owner session writer through
an observer.

Usage counts only committed assistant response messages, matching the session usage projection. `reported_requests == 0` means
usage is unavailable, not zero cost. `unreported_requests` exposes missing usage;
prompt/completion totals sum only reported values. No price or latency estimates
are synthesized. Timings derive from actual journal/task timestamps and exclude
durable pauses. Agent completion time remains its first terminal observation,
not a later conversation-close timestamp. Unfinished interrupted work has no
claimed elapsed duration when its actual end is unknown.

Cancellation checks the actual workflow agent token after native startup/turn
settlement. An aborted native TurnReport cannot become a completed graph node.

## Verification

- Actual native three-phase fixture, with all three provider responses held:
  links/session IDs/lifecycle are visible before any response, while concurrent
  projection reads and task revisions complete without a lock cycle.
- Pause, three measured/missing-usage responses, resume, actual guarded file
  write, no duplicate native requests, and history after runtime disposal.
- Actual provider failure, joined cancellation, full-token foreign authority
  refusal, paused-history cancellation without any inference/job admission,
  and observer-persistence refusal before first provider request.
- Session metadata compatibility and strict correlation/rebinding/revision/
  usage checks, including history after the native task is closed.


Final checks:

- `cargo test -p heycode-agent --lib workflow::`: **8 passed**.
- `cargo test -p heycode-agent --test main -- --test-threads=4`: **281 passed**.
- `cargo test -p heycode-session --test main`: **142 passed**.
- `cargo clippy -p heycode-agent -p heycode-session --all-targets -- -D warnings`:
  **passed**, including the final timing/label compatibility correction.
- Changed Rust files pass targeted formatting and the diff passes whitespace checks.

An initial fully parallel agent run timed out in the existing shell descendant
cancellation test; that exact test passed in isolation and the complete suite
then passed with four test threads. No assertion was weakened. TUI compilation,
visual capture and end-to-end navigation remain the UI/root integration task.

## Durable coordinator jobs for live and historical notice filtering

The follow-up based on `ceae203` records `WorkflowChange::Job` with exact run ID,
attempt and canonical JobRegistry ID. The coordinator callback appends and
flushes this association before polling the workflow worker. An admission
append/flush error prevents all workflow effects and settles the owned job as
Failed, including the JobRegistry's terminal fallback if the owner journal is
unwritable. No new scheduling mechanism or label/time correlation is used.

`WorkflowView.jobs: Vec<WorkflowJobView>` retains all exact associations in attempt
order, while the existing `job_id` remains the current live job. Each record has
`attempt`, typed `job_id`, `admitted_at_ms`, `settled_at_ms`, and optional
`WorkflowOutcome`. The first committed End for that attempt supplies its outcome
before the job inbox notice is published. A later cancellation of paused history
does not rewrite the already-paused coordinator outcome.

UI notice filtering can match owner-local `InboxSource::Job` IDs against outcomes
`Some(Completed | Paused | Cancelled)`; `Failed` and unknown outcomes remain
visible. Old logs have empty association lists rather than inferred job ownership.
Duplicate IDs across runs, multiple jobs for one attempt, wrong attempts, unknown
runs, noncanonical IDs, and retroactive admission after work are refused. Fields
are bounded and there is only one association per durable attempt.

Tests exercise direct start, pause, UI-style resume without any provider tool
result, completion and replay after live registry disposal, retaining both job
IDs and exact outcomes. A real superseded session writer proves failed admission
settles Failed without polling any native provider or writing the final report.

Follow-up validation: workflow unit tests **9 passed**, workflow session-domain
tests **9 passed**, existing workflow integration tests **3 passed**, and strict
all-target agent/session Clippy **passed**. Root/UI owns the new `Job` observation
match arm and final complete-workspace replay checks.
