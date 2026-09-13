# heycode-agent

The native heycode turn owner. It assembles requests from durable session
projection, dispatches verified provider adapters, executes guarded tools and
publishes live UI events only after their corresponding durable commit.

The live native inference route is one snapshot: provider, model and optional
reasoning effort. `Agent::set_inference_route` publishes all three together;
every request copies that same effort into its draft and durable request header.
`BackendControlOwner::{NativeInference,DelegatedRuntime}` and the captured
routing revision also travel through model/effort UI events, so a frontend
cannot apply a delegated choice to the dormant native provider or submit a
same-owner picker after its configuration changed.

## Approvals carry the user's own words

`AskAnswer::deny_with_reason` turns a refusal into a redirection: the text is
normalised (control characters flattened, whitespace collapsed, bounded by
`MAX_DENY_REASON_CHARS`) and becomes the model-visible denial
`denied by the user: …`, so "no, use the staging bucket" changes what the model
does next where a bare refusal only stops it. Blank text degrades to a plain
`Deny`. The TUI opens its one-line editor with `r` or the fourth card choice;
while it is open the card owns every key, so a typed `y` is a letter.

## Turn integrity

- One cancellation lease owns each turn; shutdown is a separate terminal
  operation.
- A06 overlaps only the explicit read-only allowlist, serializes approval, and
  commits every tool call/result in model order through one bounded cursor.
  Unknown or mutating tools are barriers.
- A07 routes every handled preparation, provider-stream, invariant and tool
  batch failure through one closure. Preparation/stream failures append
  `step/end` before `turn/end error`; pre-step/post-step failures append only the
  turn end. Failure UI publishes after this durable closure.
- C08's session repair projection is the oracle for an actual process crash;
  handled failures must leave it clean, while a real crash remains Unknown or
  Interrupted rather than fabricated success.
- C11 measures every strict `ResolvedCall` and legacy `ChatRequest` through the
  composed token-counter registry. The Agent retains only a complete
  System/Messages/Tools/ProviderState/Attachments envelope; Consumers must keep
  exact, estimated and uncounted evidence distinct.
- Detailed provider cache/context-edit metadata stays pending with other strict
  adapter events, validates against normalized Usage and commits as one
  correlated v2 session event only when Finish succeeds. Stream error or abort
  publishes none of it.
- Provider aggregate server-tool usage follows the same terminal buffer. Agent
  validates it, commits v2 `server-tool/usage` only at Finish and never turns a
  provider count into client/server call ids.
- P10 request layers run before strict adapter resolution and durable request
  commit; response layers run before normalized output reaches the accumulator.
  The auth Consumer admits a secret-free binding preview and checks resolution
  consistency; native-tool admission post-checks downstream route edits and is
  rechecked directly; optional `provider-telemetry` records closed failure
  classes only. Cancellation settles a parked layer, a response refusal cancels
  the provider operation, and native child Agents reuse the same global chains.
- C05 also bounds replay. The durable header carries the only replay evidence
  resolution owns — native features and provider-executed native tool routes —
  so verification refuses a live call claiming MORE replay safety than that
  evidence proves (`retry_replay_safety`), before any transport starts. The
  bound is one-directional: an adapter narrowing further from protocol
  evidence the header does not carry is always admitted.
- Profile picker/selection events are human control-plane signals. Native
  runtime normalization deliberately emits no model/runtime event for them;
  the TUI and CLI own validation, settlement and recomposition.
- Provider options materialize only after the catalog resolves the canonical
  model and N01 resolves the final route set. Agent passes both to
  `request_options_for`; P10 and the durable header/C05 boundary then see the
  same result. Unsupported/unproven policy is a preparation failure, never a
  static all-request fallback.
- Before options or adapter borrowing, Agent awaits `Provider::prepare_inference`
  under a child of the exact turn token and joins it on either cancellation
  edge. A returned operation provider supplies the endpoint/auth preview used
  by P10 and C02/C05; discovery inside the later stream cannot change the
  durable target. AWS and Vertex are the first Consumers.
- MCP12 rich tool output crosses a separate typed pending plane. The ordered
  commit cursor admits media through ATT01, appends `tool/rich-result`, then
  publishes typed UI/runtime metadata. Continuation text is rebuilt from that
  durable object and wrapped with MCP untrusted provenance.
- ATT04 audio stays on a separate hidden adapter plane, so ordinary protocol
  serializers cannot silently drop it. Preflight rereads/hash-checks WAV bytes,
  requires an exact-model descriptor and distinguishes Unsupported from Unknown
  before `user/attachments`. C02 commits `text|audio` and independently
  reconstructs the request. Pending output commits bytes → `attachment/added`
  → `assistant/audio` → metadata-only UI only on terminal success;
  cancellation drops pending output and leaves no association.

## A03/A05 — durable operational input

FollowUp targets the next turn; Steer and Inject target the next model step.
Submission appends only `agent/inbox/splice`. Text becomes model-visible and
publishes `UserEcho` only when a claim atomically removes that occurrence and
appends its `user/message`. Busy submissions never wake a second owner; the
running turn re-drains next-step work before settlement, then publishes one
Wake when next-turn work remains.

The TUI now consumes that contract for native sessions. Enter submits Steer,
Tab submits FollowUp and one settlement Wake invokes
`send_follow_up_cancellable`; the caller token/JoinHandle remain TUI-owned.
Agent owns queue order, durable claims and turn closure. Delegated runtime
controls are not routed through this native inbox.

## O09 — lifecycle hook attachment

`LifecycleHookPort` is the dependency-neutral product seam for UserPrompt and
Subagent lifecycle points. One effect-owned slot is attached to the Agent and
SubagentRegistry; duplicate attachment fails loud and disposal removes only the
matching generation. Fresh and queued follow-up prompts run Pre before durable
admission and Post after it; an entitled Pre refusal writes no user/turn event.
Subagent Pre runs after authority/provider selection but before provider start,
and Post runs after registry admission. Native child Agents inherit the same
slot, so prompt hooks do not silently disappear below delegation.

The report crossing this port contains only proceed/refuse and a fault count.
Contribution text never crosses it: the product adapter commits
`hook/contribution`, and request construction sees successful output only by
re-projecting the session. Handler panic is contained and counts as a
non-vetoing fault, preserving O08's “broken hook is not an outage” policy.

## CMD04 — capability-panel requests

Human-only capability navigation crosses the live UI bus as a validated
`UiPanelId`; it never enters the session or a model request. `/plugins` opens
the installed-plugin panel with no argument and retains `verbose` as the exact
attributed inventory projection. Native/delegated runtime normalization drops
panel navigation because it is front-end control state, not agent output.

## O01–O03 — native subagent modes and authority

`SubagentRegistry` selects only a provider that proves the requested independent
seed/continuation combination. Native Fresh creates a new durable child session
that sees only its prompt; ForkParent creates a hash-verified shared-prefix
lineage child; Continuable keeps the same child Agent/session for later
`send_message`. One-shot leaves no live handle.

Providers also declare whether their children inherit this agent's guards.
`inherits_parent_tool_guards()` defaults to false, so only the native provider —
which hands the child the parent's `seam/pre_tool` waterfall — is treated as
guarded, and an ambient `DelegationGate` (plan mode is the shipped one) can
refuse every other provider before a child starts.

Lifecycle ownership and caller authority are separate. The registry owns every
handle for Context disposal, while a registry-minted `SubagentAuthority` binds
an unforgeable token, opaque owner id, depth and permission to retain children.
Nested list/send/interrupt lookups filter by owner and foreign ids are
indistinguishable from unknown ids. A one-shot child cannot leave a continuable
descendant; a continuable follow-up retains its prior depth. Provider output is
also admitted: handle presence must match continuation, handle/id must match and
a duplicate live id is closed/refused before publication.

## O05/R05/R08 — background and delegated-runtime subagents

Optional `background:true` on `task` reserves an effect-owned JobRegistry row
before spawn and returns its stable id immediately. The registry retains the
JoinHandle and cancellation token. Completion, failure or cancellation appends
one durable FollowUp notice before the visible job state settles; append failure
rolls the reservation and wake token back. Default `subagent-jobs` attaches this
host only after Agent exists, keeping `subagent` independent of an Agent
composition cycle.

The native `RuntimeSession` translates committed session events into the R02
runtime vocabulary with one stateful `NativeEventTranslator`: intermediate
assistant text is `CommentaryDelta`, a held assistant message is dropped when a
`tool/call` follows it (or surfaced as commentary if the step never streamed
chunks), and exactly one `FinalMessage` — empty for an empty stop reply — is
emitted immediately before `TurnFinished`. A step that speaks and then calls a
tool is therefore R02-valid, and an empty reply never poisons the session. The
session publishes through the shared `heycode_runtime::RuntimeEventHub`, so an R02
violation fails at the producer and a subscriber attaching after the retained
window still starts at sequence zero. Permission requests for argument-free
tools carry the detail `(no arguments)` because R02 refuses an empty block.

`RuntimeSubagentProvider` adapts registered delegated runtimes into fresh
one-shot children for Codex and Claude. Each child has its own durable heycode
session and exact `runtime/linked` record. Raw runtime events pass through R02;
permission requests use the parent ApprovalPolicy as AllowOnce/Deny,
interactive questions fail instead of receiving fabricated answers, caller
cancellation invokes the runtime and every outcome closes quiescently. Runtime
tool actions remain runtime-owned and are not relabeled as heycode client tools.

## O06 — exact-base isolated worktree provider

`GitWorktreeManager` accepts only a full lower-case SHA-1/SHA-256 commit id and
an absolute managed root disjoint from the canonical repository. Every Git
operation goes through the composed `SubprocessService` as exact argv with an
empty explicit environment, disabled hooks/fsmonitor/credential helpers,
bounded output and deadline—there is no shell command string. Creation journals
`creating` before Git and `ready` before lease publication. Success/cancel
removes the checkout synchronously; an explicit failure may be retained by
policy. Dropped or crash-left creating/ready rows recover on the next pass,
while retained failures require an exact-id cleanup.

`WorktreeRuntimeSubagentProvider` is an O01 fresh one-shot Provider. Registry
authority/depth gates still run before it creates an exact-base detached
checkout; the existing delegated `RuntimeSession` adapter runs with that path,
settles/closes, and only then may the lease clean up. Context disposal cancels
the operation owner and never spawns cleanup from `Drop`; a hard-drop leaves
the journal as the recovery handoff.

## O07 — durable authority-scoped teams

Plugin/service `teams` contributes one `team` tool over `team/change` session
truth. Root authority alone creates rosters, adds its live continuable children,
defines the task DAG, dispatches work and performs explicit recovery. Child
authorities can exchange peer mail and claim only their own messages; foreign
registry tokens and sibling probes fail without disclosure. Every operation
uses global revision CAS, tasks add task-local CAS, cycles and premature starts
fail, and crash-left `in_progress` work becomes `blocked` rather than completed.

Task dispatch reuses the live `SubagentHandle`, Agent `JobRegistry` and A03
inbox. `in_progress` commits before child input; terminal team state commits
before the ordinary job notice and visible terminal row. Wait-for-change uses
revision rechecks, coalesced notifications, at most 64 waiters and a 60-second
ceiling—there is no detached wake task. The tool returns bounded roster/task/
mail snapshots; a dedicated TUI roster panel remains a root integration task.

## O14 — isolated structured reviewer

Plugin/service `reviewer` contributes `report_findings` and `/review-runtime
<runtime> [instructions]`. Selectable rows must be delegated runtimes with exact
permission callbacks. The service commits `review/change started` with the
full base/patch/instructions, applies that patch to an O06 checkout, records an
exact Git-status baseline, runs the existing durable one-shot RuntimeSession
under `DenyAll`, and accepts only a strict JSON summary/finding schema. A second
status snapshot must be byte-identical. Mutation, invalid output, runtime,
workspace and cancellation paths commit a closed failure and publish no
findings; the worktree and runtime are synchronously settled first.

Background review reuses JobRegistry/A03: structured settlement commits before
the bounded review notice enters the parent inbox and before terminal job
publication. Root composes the service and `/review-runtime`; if its configured
state root is canonically inside the repository, it derives a workspace-id
scoped sibling root rather than blocking startup. The TUI-owned active-route
`/review` remains a distinct command rather than silently changing semantics.

`report_findings` is the direct model-reporting path. Its input contains only
severity, the same cwd-relative file path used with `read`, a line location, the
exact SHA-256 revision returned by `read`, and bounded control-free
title/trigger/failure/impact text. The durable record normalizes that path to
the selected workspace root.
The service pins the current workspace generation, derives session/root/cwd
source identity from the live owner, rechecks every unique file revision and
maximum referenced line, then appends and flushes one `review/change`
`findings_reported` event before emitting `UiEvent::FindingsReported`. Exact
duplicate defects are refused; distinct defects may share a location. The
operation is local and does not publish or send findings externally.

## E09/CMD08 — execution jobs and control commands

Plugin `execution-jobs` binds the composed shell and terminal services to the
Agent-owned `JobRegistry`. Shell defaults resolve before the job exists;
terminal jobs convert that exact resolved process into interactive PTY mode.
The registry owns every task/token, cancellation reaches the subprocess tree,
and `Agent::settle_job` commits one source-attributed inbox notice before the
terminal job state becomes visible. A terminal waiter remains visible to the
same owner in `/ps` until settlement; a concurrent hard kill joins that waiter
and returns only after the tree is quiescent.

The same plugin contributes human-only `/tasks`, `/ps`, and `/stop`. They read
the live job/terminal owners directly—there is no second process model—and do
not append user messages. `/stop` accepts one job id, one owner-scoped terminal
id, or `all`; execution-managed terminals route cancellation through their job
token so the waiter and durable notice settle exactly once. Model-facing
`background_shell` and `background_terminal` are the product Consumers that
start these rows; ordinary `list_jobs`/`cancel_job` control them afterward.

## O10/CMD11 — durable goals and bounded rounds

Plugin/service `goals` projects version-one `goal/change` full snapshots and
clear tombstones from JSONL. Every mutation advances one exact `GoalRef`
revision and validates lifecycle/counter/timestamp continuity before append.
Activation is process-local: create/resume arms it, pause/block/complete/clear
disarm it, and every composition over an existing session starts disarmed.

The round driver checkpoints the session before reserving a source-tagged A03
FollowUp. Only the atomic claim plus `user/message` advances `rounds_started`;
human messages do not. Definition round limits and a per-activation consecutive
wake budget bound automatic continuation. `AgentIdle` is emitted only after the
turn cancellation lease drops (while the turn gate is still held), giving the
driver an authoritative enqueue checkpoint rather than a spinner inference.
The model `goal` tool and human `/goal` command both enforce CAS. `/goal resume
<message>` deliberately schedules exactly one ordinary logged model input;
other command text remains human-plane only.

## O11 — plan pending/commit lifecycle

`PlanMode::set` commits immediately between turns. During an open turn it keeps
one process-local target and the Agent commits it at the next accepted pre-step,
before `step/start` and request assembly. Reversing a pending selection to the
already logged value cancels it without a phantom `plan/mode`. The review tool
works only while plan mode is active; approval queues/commits the exit through
that same boundary. `/plan <message>` first selects plan mode and then invokes
the ordinary Agent send path, so the optional text is durable exactly once.

Enforcement is default-deny in two places, because one is not enough. The
`seam/pre_tool` guard admits an explicit read-only allow-list and denies
everything else, so a tool composed after the guard shipped — `background_shell`,
`workflow`, any `mcp__*` tool — is denied rather than exempt. `task` is on that
allow-list, but admission only covers the native child, which is built with the
parent's own `pre_seam` and so inherits the guard. A delegated child is a
separate agent, routinely a separate process, that never runs through this
waterfall, so `PlanMode` also installs as the `SubagentRegistry`'s
`DelegationGate` and refuses to start any provider reporting
`inherits_parent_tool_guards() == false` while plan mode is on — before the
child exists. Whichever of `plan`/`subagent` composes second installs the gate,
so either order is covered and a second claimant fails loud.

## O12 — checkpointed workflows

Plugin `workflows` separates a `WorkflowWorker` Provider from the model-facing
`workflow` Consumer. Version-one definitions declare their required host
capabilities and contain bounded deterministic steps. The default sequential
worker supports progress plus cancellable bounded delays; another worker may
replace it without changing the service/tool contract.

Runs are effect-owned JobRegistry operations. `workflow/change` records start,
contiguous progress, completed-prefix checkpoints, explicit resume attempts and
terminal outcomes. Resume starts strictly after the latest checkpoint, so the
completed prefix is never re-executed. Cancellation uses the job's one token;
the worker settles, `workflow/end` commits, and only then does the job notice
publish. A crash may leave a legal running prefix, which is explicitly
resumable rather than silently restarted.

## O13 — durable schedules

Plugin/service `schedules` owns session-local one-shot delay, absolute-time and
fixed-rate records plus `schedule_create|list|delete`. Every read/mutation first
forces `Session::flush`. Due delivery writes a source-tagged A03 enqueue,
appends the correlated `schedule/change dispatch` only after that enqueue
succeeds, flushes again, and only then publishes the wake. Recovery detects the
narrow enqueue-without-dispatch crash window by message id and finalizes it
without duplicating the input.

Timers are token/JoinHandle-owned effects, split long waits into bounded wall
clock rechecks, and rebuild from JSONL on resume. Fixed-rate catch-up selects
the latest due occurrence and advances directly to the first future anchor.
Forks project only events at or after `Session::first_local_seq`; inherited
pending schedule inputs are cancelled, and `copy_inherited` is the explicit
new-id operation when the child should retain reminders.

## TEL04 — committed metrics

Optional default effect plugin `telemetry-metrics` injects the authoritative
session and whichever telemetry Provider the profile selected. It reads the
existing prefix only to seed bounded lineage, runtime and request-route
correlation; it does not replay historical metrics. New observations arrive
from the session-owned post-commit bus, so a failed append cannot become an
exported success fact.

The Consumer emits only closed provider-request, tool, compaction and cache
metrics. Local calls, exact provider calls and provider aggregate usage carry
distinct execution labels; aggregate request counts survive into telemetry
schema v2 rather than collapsing to one. Purpose, route, lineage and runtime
come from durable typed records. Prompts, tool arguments/results, paths,
response bodies and arbitrary errors have no event field. Four exact
`telemetry_metric` inventory rows and the listener dispose with Context.

## A08 — deferred tools and Code Mode

`DeferredToolCatalog` unions current client schemas with resolved N01 routes.
An injected `DeferredToolProvider` selects names for one request; the Agent
applies the same selection to tool schemas, native routes and prompt tool-name
context before dispatch. Selection failure is a preparation failure—there is no
silent full-catalog fallback.

The large-catalog fixture uses exact serialized schema bytes and JSON node
counts as deterministic context/work proxies. Selecting three rows from 5,000
reduces both by more than three orders of magnitude without a wall-clock
assertion.

`CodeModeSchedule` can materialize legacy chunks or strict inference events,
but it has no tool registry or executor. Providers emit ordinary tool calls;
A06 remains the sole approval/execution/durable-commit owner. Optional provider
state needed by a specific protocol remains that adapter's responsibility.

Default root composition installs `LexicalDeferredToolProvider` with an explicit
64-row ceiling. Small catalogs pass through unchanged. Only larger catalogs rank
query overlap with stable registry-order ties; selection failure/cancellation
still refuses preparation rather than restoring the full catalog.

## A09 — loop budgets and hygiene

`LoopBudgetLayer` re-projects consumed state from JSONL before every step:
completed requests, reported prompt+completion tokens, dispatched client tool
calls and elapsed time since `turn/start`. It stores no counter, so a resumed
session reaches the same next-step decision. Missing usage is either a
fail-closed stop or an explicit lower-bound policy.

All default turn limits are disabled: steps, cumulative tokens, elapsed time,
and tool calls. Zero means unlimited. Explicit policies remain available to
callers that intentionally configure them; explicit timers above 24 hours are
rejected. Provider-native continuation has no separate hidden turn cap.
Cancellation and model context/output constraints remain authoritative.

`loop-budget-settings` is restart-applied. An unlimited policy bypasses budget
projection entirely; explicitly enabled token accounting uses reported
prompt-plus-completion tokens across requests, not the size of the current
context. Missing usage matters only when a token limit was explicitly enabled.

## C12 — compaction transactions

Default plugin `compactions` publishes three explicit rows: `provider-native`,
`portable-summary` and `prune-oldest`. Strategies prepare read-only plans; the
registry verifies no strategy wrote the log, rejects a concurrent mutation and
alone appends one durable settlement. `/compact`, automatic pressure and the
native RuntimeSession all dispatch through that registry.

Portable summaries work through legacy or strict inference with request purpose
`Compaction`. Native dispatch additionally requires exact catalog support and an
adapter-owned transport operation. Its opaque checkpoint commits as
`compaction/native`; only the same provider/model/protocol route replays it.
Neutral and incompatible routes retain the original append-only history.
Cancellation settles the provider future before return, and shutdown disposes
all strategy rows.

C14 adds explicit preparation for provider/model switches over opaque state.
`Cancel` changes nothing; `ForkBeforeCheckpoint` commits a shared-prefix child
immediately before native settlement and leaves the current session/route
unchanged; `PortableRecompact` uses the current provider, verifies the later
portable marker cleared the barrier, and only then authorizes routing commit.
Turn completion is matched by turn id, so checkpoint/metadata events after a
`turn/end` do not make the turn appear open.

`/compact` now accepts `list`, `<strategy> [keep]`, or the historical numeric
`[keep]` shorthand for `portable-summary`. Listing reads live registry
descriptors and writes no session/model state.

## Remaining provider integration

Provider-specific Code Mode adapters must retain whatever opaque continuation
state their protocol requires while emitting the ordinary calls here. Exact
profiles may omit either default effect plugin; they then keep the full catalog
and legacy eight-pause compatibility cap rather than acquiring hidden policy.

## Q04 — persisted request replay oracle

`heycode_agent::testing::verify_persisted_replay` accepts a seeded fixture Session
and one live `ResolvedCall`. It commits the exact C02 header/context, flushes and
reopens physical JSONL, selects the exact request id from `project_requests`,
then returns only after ordinary C05 verification against the exact adapter.
Errors carry stable stage/field information and never request content.

The shared fixture matrix covers Chat Completions, Responses, Anthropic
Messages, Gemini GenerateContent and Bedrock Converse. A physical-byte
corruption regression proves the oracle cannot accidentally use the in-memory
Session; the first attempted mutation appended at the live writer's cursor and
was overwritten, so the effective mutation overwrites an existing persisted
byte before the oracle runs.

## QSEC05 — strict untrusted-content projection

The strict C02/C05 input mapper derives one source-specific model wrapper before
constructing a provider message. Every role must consume that derived value;
the Tool arm may not recopy the raw durable body after the boundary was applied.
A focused regression enumerates Web, MCP and LSP and keeps an unlabelled legacy
tool result byte-exact with the same call/error metadata.

The black-box product gate seeds real public-format sessions, observes each
wrapped hostile source at a loopback provider, receives a genuine `write` tool
call, requires deny settlement with no mutation and complete cleanup, then runs
an independent auto-approval control that must create its isolated marker. The
source warning is model context; the approval policy remains the authorization
mechanism.

## Focused verification

```sh
cargo fmt -p heycode-agent -- --check
cargo clippy -p heycode-agent --all-targets -- -D warnings
cargo test -p heycode-agent --no-fail-fast
```

## Plan review and implementation permissions

Plan is a native, provider-independent read-only mode. Enter with
`/permissions plan` or `/plan on`. Entry immediately blocks new mutations and
settles existing background execution before announcing Plan fully active.
A failed settlement stays read-only and can be retried.

`exit_plan_mode` presents a complete Markdown proposal through a dedicated human
review, independent of generic tool approval or Full access. Acceptance chooses
Accepted edits or Default permissions; rejection, feedback, Escape and failed
transitions keep Plan active. Policy selection and Plan exit share one durable
`plan/review` record before implementation can start. The saved proposal and
feedback survive resume; `/plan review` opens the saved full document again.
An unavailable resume policy fails closed. `/plan off` cannot bypass review.

Frontends must advertise full plan-review capability and answer with
`PlanReviewDecision` through `InteractiveApproval::answer_plan`. Ordinary
Allow/AllowSession responses never resolve a plan review. Native hosted tool
routes with unproven read-only behavior are rejected before provider dispatch.
## Portable session commands

`/btw <question>` answers a tool-free aside using bounded current context, even
while native work runs. `/recap` gives a one-sentence recap; `/recap off` disables
automatic return recaps for this session. Both use the selected provider's normal
prepared inference path and may incur provider usage (up to 2,048 output tokens).
Their output stays out of the main conversation log.

`/output-style default|concise|explanatory|learning` saves additional response
instructions for the next native request. `/output-style custom <instructions>`
accepts up to 8 KiB and retains the base coding and permission prompt.

`ask_user_question_async` persists an optional question and immediately returns
its id. `/questions` lists pending questions, `/answer <id> <text>` admits an
answer exactly once to the durable follow-up queue, and `/questions cancel <id>`
dismisses it without inventing an answer. Required questions still use the
blocking `ask_user_question`. Inherited tools bind optional questions to the
actual executing child session. Delegated runtime configuration omits this tool
until that runtime has an answer-delivery path.


Plan also gates configured hooks, which do not declare read-only behavior. An
operation requiring one of those hooks is refused while Plan is active; the hook
is never silently treated as having approved. Entry waits for in-flight hooks
and actual worker completion, including code after a job's settlement callback.

## Native workflow graphs and teams

The default `workflows` composition now mounts `native_workflow_plugin`.
Version 1 definitions retain their ordered Emit/Delay behavior; version 2
executes a dependency graph using the current native host's guarded tool path
and fresh native one-shot children. The native LLM route may use any configured
inference provider; this feature does not depend on a subscription CLI runtime.

Definitions may include an explicit `title` and ordered `phases` with stable
`id`/`title` pairs; every step then declares its `phase_id`. Omitted metadata keeps
one phase per step. Grouping is presentation metadata and preserves the explicit
dependency graph. The [three-phase example](../../docs/examples/workflow-sequence.json)
runs three native agents in parallel in its middle phase.

`WorkflowService::views` provides read-only run, phase, node and agent progress.
Actual public task IDs and native session IDs are durably linked before first
inference. Historical summaries and reported token usage remain in the owning
workflow journal after handles close. Missing usage stays explicit. Owner-checked
`views_for`, `view_for`, `pause_for`, `resume_for`, and `cancel_for` validate the
full native authority; viewing progress never starts work.

`workflow` exposes the complete definition schema. A step has `id`, `label`,
`action`, optional `depends_on`, `when`, `max_attempts` and `replay_safe`.
`max_parallel` is 1–8 (default 4); definitions contain at most 64 nodes. Tool and
agent steps declare the corresponding `tool` or `agent` capability alongside
`progress`. JSON arguments and results support an exact binding object
`{"$ref":"dependency#/json/pointer"}`; `dependency#` binds its entire result.
All referenced steps must be explicit dependencies. Conditions resolve to a
boolean, or compare bound JSON values with `{"equals":[left,right]}`. False conditions and skipped dependencies propagate a skipped node;
failed dependencies prevent dependent effects. Retries are bounded to five
attempts, include bounded backoff, and require `replay_safe=true` for effects.

Each graph effect has an intent flushed before invocation and a result flushed
before dependent work. Pause is cooperative: `workflow(action="pause")` requests
a stop after the currently admitted batch settles; `list` reports `paused` only
after that durable boundary. Resume skips every committed completed/skipped
node and retains retry counts. A crash-left intent has an unknown effect
outcome and is refused on resume even when the step was declared replay-safe;
inspect the effect and deliberately start replacement work after reconciliation.
Native child callers execute workflows in their own conversation. Admission
captures the caller's tools, guards, approval, cwd and shutdown owner; agent
steps preserve the actual native parent and authority. Completion notices go to
that owner's inbox. Owner shutdown cancels and joins admitted work.

Forks cannot inspect or resume an ancestor's runs or use inherited saved
definitions. Cancel uses the ordinary job controls and joins admitted effects.
`save`, `saved`, and `run_saved` manage a bounded durable session-local library;
each run retains its immutable definition copy. Status and libraries use only the
owner's local journal, and workflow output has the orchestration trust boundary.

For example, this version-two definition writes a native child's result:

```json
{
  "version": 2,
  "name": "research-note",
  "description": "Produce a note and save the result",
  "capabilities": ["progress", "agent", "tool"],
  "steps": [
    {"id":"draft","label":"Draft note","action":{"kind":"agent","prompt":"Read the project README and write a short explanation."}},
    {"id":"save","label":"Save note","depends_on":["draft"],"action":{"kind":"tool","name":"write","arguments":{"path":"note.md","content":{"$ref":"draft#"}}}}
  ]
}
```

Team creation defaults to native worker and reviewer roles. Supply `roles` with
`display`, `role` and `instructions` for a custom roster, or explicit `roles:[]`
for manual member addition. Roles are admitted in one roster event after all
children are ready; a startup failure closes this call's newly started children.
A failed creation can leave the lead-only team for inspection. `create_task`
automatically dispatches ready work unless `auto_dispatch:false` is explicit.
Completion releases dependencies, including dependency result summaries, and
at most one task per member executes at a time. `dispatch_ready` dispatches only
Pending tasks. Crash-left/cancelled Blocked tasks require explicit dispatch;
recovery cannot relabel a task still owned by a live job.

`send_mail` inserts durable peer mail, admits it once to the recipient's inbox,
and records delivery. Native child inbox turns run automatically through the
same JobRegistry and child turn gate; active conversations finish their current
turn before consuming the follow-up. A recipient's explicit `claim_mail` is a
separate acknowledgement. Retrying `deliver_mail` bridges interrupted delivery
without reinserting already-admitted input. Unsupported or unavailable recipient
handles leave the message durable with an actionable delivery error. Team model
snapshots expose only mail sent by or addressed to that caller. Human UI callers
can use `TeamService::projection`, `TeamView::mail` and `render_team` for the full
authoritative task/member/dependency/result/delivery/claim view.

## Custom child configuration and native inspection presets

`SubagentPreset::with_config` attaches a validated `SubagentConfig`.
`SubagentRequest::with_preset` snapshots its configuration, separate standing
instructions and source identity for persistent memory. Alias changes preserve
that identity. Native children apply model/effort overrides before their first
request; retained follow-ups use the same immutable configuration and lifetime
step counter.

A narrowed child receives a filtered ToolRegistry with `ScopedChildTool`
wrappers. Wrappers preserve effect classification, rich results and untrusted
content provenance while checking both tool names and arguments. This lets
CodeMode or other guarded callers reuse the actual child registry without
bypassing skill scoping. The Agent also checks its own call admission and
execution. Provider-hosted routes and inherited executable hooks are disabled
under a narrowed configuration. Parent guards and approval policies remain
in force.

Default native presets `reviewer`, `advisor`, and `security-review` use fresh,
read-only children with a bounded inspection tool set and a 32-step lifetime
limit. They inherit the configured native inference route and are callable via
`task(agent=...)`. Token-owned fallback registration allows user/project
presets to shadow them and restores built-ins when overrides are withdrawn.
The contribution inventory records these defaults as `agent_preset_fallback`,
matching their separate registry; ordinary `agent_preset` ownership remains
exclusive even when an identically named fallback is present.
No external CLI subscription is required.

See the extension-host README for the JSON schema, import boundaries, memory
scopes and `/agent-config` authoring/reload commands. External providers opt
into `supports_configuration()` only when they actually enforce the separate
instruction and policy contract; the default refuses configured requests.

## Execution jobs and local monitors

The default execution plugin gives ordinary model bash calls a job ID before
launch and observes stdout/stderr while the process runs. Foreground promotion
changes delivery on that same job; it does not launch a second process.
`background_shell`, `background_terminal`, and `run_tool {tool, arguments,
background}` expose explicit jobs. `run_tool` targets must implement
`Tool::supports_background`; target approval, guards, cancellation, and rich
result admission still run in the actual caller's context.

`job_output {job_id, stream, offset, limit}` pages a bounded retained tail
without consuming it. Streams are `stdout`, `stderr`, and `terminal`; pages
report total bytes, retained start, and lost bytes. Default retention is 256 KiB
per stream, with a 6 KiB inline preview and 64 output records. Configure
`tools.execution_retained_bytes` (1024..1048576),
`tools.execution_inline_bytes` (1024..32768), and
`tools.execution_history_limit` (1..256). Output files beside the session are
private atomic checkpoints at admission and settlement. Settled tails survive
resume; a crash interrupts unfinished metadata and may lose all live bytes
since admission. These files are bounded tails, not complete process logs.

`monitor` accepts a command or `job_id` for existing output. Its `watch` object supports
`contains`, `exclude`, `stream`, `ready_when`, `stop_when`, `debounce_ms`,
`min_interval_ms`, `dedupe_ms`, `max_events`, `timeout_ms`, and `stop_source`.
It delivers bounded untrusted-text batches to the caller inbox through the
shared wake budget. Eight coordinator slots are independent of process slots;
per-watch event and lifetime limits do not replenish with turns. Cancelling a
watch cancels a command it started. Existing sources stay independent unless
`stop_source` is set. Readiness markers also work without a newline. Supported
sources are local commands and retained execution output; there is no WebSocket
source.

A child inherits its own registry, session, inbox, cancellation owner, and cwd.
Worktree rebinding replaces shell and PTY process authority while preserving
sandbox mode. Terminal IDs remain in the common registry under the caller's
host-assigned owner. Model reads stay scoped; the human UI may inspect child
output and send input using `ExecutionJobService::write_terminal`.
`/stop` acknowledges immediately and waits at most five seconds for confirmed
settlement, explicitly reporting an unconfirmed timeout.

## Native task lifecycle and isolation

`task` allocates an opaque `task-*` identity before inference. `list_tasks` returns all admitted
child snapshots (including one-shot results) plus the shared request budget.
`session_id`, `job_id`, `workspace`, revision and state are separate fields;
provider session identifiers are never substituted for the public task ID.
Native observers attach to the child Agent before its first provider call.

`oneshot`, `continuable`, `fork` and `fork-continuable` select context seeding
and retention independently. Foreground task calls in one model batch overlap
through `ToolEffect::Orchestration`; their durable results retain model order.
Native first turns and follow-ups propagate cancellation through inference and
tools. Interrupt means cancelling until execution settles; archive retains a
settled continuable conversation, restore reopens that retained handle, and
close joins shutdown. Restarted handles are reported as Interrupted and are
not silently replayed or reconstructed. Finished one-shot sessions remain in
durable history but do not acquire a continuable handle on restart.

`send_message(background=true)` returns immediately with a job ID and, once a
native session exists, a durable inbox message ID. `steer=true` uses the native
step-boundary queue without cancelling the active turn. The job reservation
precedes inbox admission, an owned driver handles idle races, and queued
cancellation removes unclaimed input. External providers without this inbox
contract refuse steer; their background follow-ups retain the owned-job path.
`interrupt_task(action=wait)` waits at most 60 seconds for a snapshot revision.
Nested background completions can resume a continuable native parent without a
UI driver. Resume coordinators use the shared job/request limits, honor wake
demotion, and settle without emitting recursive completion notices.

`isolation=worktree` snapshots current tracked changes and bounded untracked
files into a native Git lease. A continuable child retains the lease across
follow-ups; terminal cleanup keeps tracked, untracked, ignored and committed
changes. The snapshot's workspace identifies the reviewable result directory.
Core file and shell tools retain the parent sandbox mode with the workspace
rebound to that lease. Host Git lease management uses fixed argument vectors
outside the model command interface. Parent model/effort and actual parent
workspace/tools are resolved at spawn time, including nested background tasks;
custom explicit overrides apply after that inheritance.

Job admission, history retention and descendant provider-request/output limits
are configurable. Native inference permits are released before tool execution,
so nested children cannot deadlock by waiting while all permits belong to
parents. Request reservations survive restart; they bound dispatched requests
and output requests, not provider billing in dollars. Jobs and task projections
persist bounded summaries and recover process-owned work as Interrupted.

Zero native inference concurrency, lifetime provider-request, job-admission, and output-token limits mean
unlimited and are the defaults. Built-in inspection agents do not set max_turns.
Goals also default to unlimited rounds and continuation wakes. Optional explicit
limits remain supported. Concurrency, queue size, and retained-history capacity
still bound simultaneous resource use; they do not impose a lifetime turn budget.
Native inference concurrency has no configured count limit by default; an explicit
positive value bounds simultaneous requests. The CLI accepts process-job concurrency
1..256, job queue 0..4096, retained job history
1..4096, and output tokens 0..131072. With no output cap, provider/model defaults
apply; explicit caps retain normal model validation.

Workflow execution is opt-in for each human turn. `ultracode` and `workflow`
request workflow use for the current task; ordinary work does not start a
workflow by default. The shared system instructions reach native and delegated
runtime configurations. Questions, quoted examples, and negations do not grant
permission to start a workflow.
