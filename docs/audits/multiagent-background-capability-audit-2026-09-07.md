# Multiagent and background execution capability audit

Audit date: 2026-09-07. Scope: the current dshx working tree and debug binary, shipping CLI composition, native agent, external runtime adapters, TUI, custom declarations, jobs, workflows, teams and schedules. This is an audit, not an implementation or a claim of complete product parity coverage.

**dshx has substantial backend foundations, but the requested multiagent experience is incomplete.** Custom presets, native subagents, background subagents, background shell/PTY jobs, durable completion delivery, team task dependencies and scheduled prompts already exist. The largest gaps are live visibility/control, early child identity, cancellation, streaming monitoring and usable agent workflow execution. Adding blue styling alone would leave those problems intact.

Evidence labels: **Live** means exercised through the real binary on a PTY with a local HTTP provider fixture; **Source** means directly traced in the shipping implementation; **Test** means existing automated tests passed. A missing capability means absent from the audited native/default path; plugins or external coding-agent runtimes may offer additional capabilities.

## What is actually available

| Capability | State | What the implementation does |
|---|---|---|
| Plan mode and permission handoff | Partial | Read-only guard and `exit_plan_mode` exist, but the requested three-choice plan review and explicit permission transition are missing. |
| Custom subagents | Partial | User and trusted project JSON presets select instructions, display name, provider and mode. |
| Native subagents | Present | Fresh one-shot, fresh continuable and parent-context fork modes; durable child sessions. |
| Background subagents | Present with defects | `task(background=true)` returns a job ID and later delivers a completion notice. Early inspection and cancellation need work. |
| Follow-up to a child | Partial | `send_message` awaits a reply from a retained continuable child. There is no interactive child conversation switcher. |
| Background shell | Present | `background_shell` starts a command, captures bounded output and notifies on completion. |
| Background PTY | Partial | `background_terminal` starts a persistent PTY, but its read/write tools are opt-in while the start tool ships by default. |
| Job listing/cancellation | Present with defects | `list_jobs`, `cancel_job`, `/tasks`, `/ps`, `/stop`; summaries rather than an actionable task dashboard. |
| Monitor-style event streaming | Missing | No shipped tool that turns each matching output event into an in-session model notification while a process stays alive. |
| Parallel tool execution | Partial | Up to eight read-only calls overlap; effectful calls are barriers; display remains ordered. |
| Agent workflow executor | Missing in default worker | `workflow` exists, but its worker executes only Emit and Delay steps. |
| Teams | Partial | Durable roster, dependency-aware tasks, background dispatch, mailbox and revision checks; no team console or automatic mailbox conversation delivery. |
| Scheduled work | Partial | Durable session schedules and resume recovery; not a detached daemon that runs after the app closes. |
| Worktree agents | Partial / preservation risk | Optional external one-shot providers; successful completion removes the worktree without a built-in result-patch handoff. |

## Verified tool inventory

The isolated composition audit declared **30 client tool names**. The real OpenRouter fixture request advertised **29 client functions plus `openrouter:web_search`**, because search was routed to the provider-native tool. This is stronger evidence than counting Rust files. It does not establish that every tool is usable on every provider, profile or external runtime.

| Area | Tools in the default native composition |
|---|---|
| Files and shell | `read`, `write`, `edit`, `bash`, `glob`, `grep` |
| Planning and input | `todo_write`, `exit_plan_mode`, `ask_user_question`, `goal` |
| Web | `web_fetch`, `web_search` — search can be replaced by a native provider route |
| Code intelligence | `lsp_servers`, `lsp_definition`, `lsp_references`, `lsp_diagnostics` |
| Skills | `load_skill` |
| Subagents | `task`, `send_message`, `list_tasks`, `interrupt_task` |
| Background execution | `list_jobs`, `cancel_job`, `background_shell`, `background_terminal` |
| Orchestration | `workflow`, `team` |
| Scheduling | `schedule_create`, `schedule_list`, `schedule_delete` |

Optional terminal tools: `terminal_open`, `terminal_write`, `terminal_read`, `terminal_resize`, `terminal_kill`, `terminal_list`. Enable with `[tools] terminals_enabled = true`; default is false. MCP/plugin contributions and hosted provider tools are additional, configuration-dependent inventory. A registered external runtime is not proof that its executable or credentials are ready.

Relevant human controls: `/agents`, `/tasks`, `/ps`, `/stop`, and Ctrl+B to cycle Diff → Tasks → Agents → closed. `/agents` is a provider/preset catalog. `list_tasks` lists retained continuable children. `/tasks` prints job summaries. These similar names currently refer to different objects.

## Detailed issues and missing capabilities

Priority indicates implementation order for the requested experience, not a security severity score. P1 should be addressed before relying on autonomous editing/background orchestration; P2 covers core product functionality; P3 covers polish and compatibility.

### Execution and correctness

1. **P1 — Native first-turn cancellation is not propagated (Live + Source).** `NativeSubagentProvider::start` checks the token before starting, then calls `run_child`, which calls `child.send(prompt)` without the caller's cancellation token. The held HTTP child stayed active after `/stop job-0`; the stop completed only after the fixture released the response. Follow-up turns do use `send_cancellable`. Fix the initial turn and test cancellation during provider streaming and tool execution. [Source](../../crates/heycode-agent/src/subagent.rs#L283).

2. **P1 — `/stop` can wait without a timeout for that broken cancellation (Live + Source).** It signals the job and waits for settlement. The terminal showed no stop acknowledgement during the two-second held response; after release it showed `stopped job-0 (cancelled)`. Provide an immediate cancelling state, bounded escalation and explicit failure if a worker will not stop. Do not report final cancellation just because a token was signalled. [Command](../../crates/heycode-agent/src/execution_jobs.rs#L392), [wait loop](../../crates/heycode-agent/src/jobs.rs#L425).

3. **P1 — Background PTY start and management defaults disagree (Live inventory + Source).** `background_terminal` is advertised, while all six `terminal_*` tools are absent by default. `/ps` reveals an ID but no output. A model can launch a server and lack the normal tools to inspect or interact with it. Register the required management tools together, or reject the launch with an actionable explanation. [Defaults](../../crates/heycode-tools/src/config.rs#L31), [start tool](../../crates/heycode-agent/src/execution_jobs.rs#L240).

4. **P1 — Worktree editing results have no guaranteed handoff (Source).** Shipping optional worktree providers use `RemoveAlways`; `WorktreeSubagentProvider::start` cleans the lease before returning the textual result, with no automatic patch export/branch handoff in that path. Uncommitted work left only in that worktree can be removed. This is a code-path risk, not a live external-agent loss reproduction. Preserve changes or export a verified artifact before cleanup. The manager's separate `apply_patch` method applies an input patch; it does not export the child's result. [Composition](../../crates/heycode-cli/src/lib.rs#L2888), [cleanup](../../crates/heycode-agent/src/worktree_subagent.rs#L268).

5. **P2 — No visible child ID until the first turn ends (Live + Source).** Background startup returns a job ID; a continuable handle is retained only after provider `start` completes. The live child was absent from `/agents`' count while running. Register a child in Starting state before inference, correlate job/session/child IDs and expose that identity immediately. [Registry](../../crates/heycode-agent/src/subagent_provider.rs#L1125).

6. **P2 — One-shot agents disappear from the live-child inventory (Source).** `list_tasks` covers retained continuable handles, not all starting/running/completed children. A complete task tree needs historical records and state independently of whether a conversation handle remains open. [Tool](../../crates/heycode-agent/src/subagent.rs#L662).

7. **P2 — Ordinary foreground subagent calls serialize (Source + Test).** `task` does not declare a read-only effect, so it is a scheduler barrier. Asking for multiple foreground children in one model response does not make them overlap. Background startup calls can finish quickly and allow actual child overlap. Add explicit orchestration semantics without falsely marking editing agents read-only. [Scheduler](../../crates/heycode-agent/src/schedule.rs#L56), [task](../../crates/heycode-agent/src/subagent.rs#L416).

8. **P2 — No general background option for arbitrary tool calls (Source).** Shell, PTY, subagent and team/workflow paths have dedicated background support. An ordinary MCP tool or other long-running tool cannot be promoted through one common background-job interface. Define supported background effects and cancellation/output contracts.

9. **P2 — No foreground-to-background promotion control (Source).** Ctrl+B cycles side panels. It does not background a currently running shell call or child. Add a distinct, discoverable action and preserve the original execution identity. [Keymap](../../crates/heycode-ui/src/keymap.rs#L381).

10. **P2 — No shared concurrency/spend admission budget for the job tree (Source).** The job registry admits tasks without a configurable parallel-job ceiling or queue. Subagent depth is limited, and foreground tool batches are capped, but those are not aggregate limits on background children, tokens or cost. Add concurrency limits, queue state and a parent-visible budget. [Spawn](../../crates/heycode-agent/src/jobs.rs#L309), [depth](../../crates/heycode-agent/src/subagent.rs#L133).

11. **P2 — Background runtime does not survive process shutdown (Source).** Jobs and handles are process-owned; durable notices, session logs and schedule/workflow projections do not restore arbitrary running processes. Expose interrupted state on resume and distinguish resumable definitions from surviving execution. [Lifecycle](../../crates/heycode-agent/src/jobs.rs#L461).

12. **P2 — Settled job history is process-local and not visibly bounded/managed (Source).** Registry rows remain available for the context lifetime; there is no user-facing archive, pruning or durable run-history explorer. Long sessions need retention limits and persisted summaries, separate from live execution handles. [Registry](../../crates/heycode-agent/src/jobs.rs#L210).

### Background output, monitoring and controls

13. **P1 — No live child conversation switcher (Live + Source).** Neither the provider/preset catalog nor the Agents side panel presents selectable live child conversations. Required behavior: select child, inspect its transcript, steer/interrupt it, and return to the parent without reconstructing the runtime. [Panel](../../crates/heycode-tui/src/side_panel.rs#L298).

14. **P2 — No persistent bottom task strip (Live + Source).** There is no always-visible row of pending agent/tool jobs below the composer with counts and expandable entries. The screenshot shows a transcript line and ordinary footer. Implement semantic states and keyboard/mouse interaction first; blue can indicate running/pending, with text/icons for accessibility.

15. **P2 — Task panels lack execution actions (Source).** The side-panel model is bounded read-only text. It has no row action to open output, cancel, retry, resume, inspect approval or jump to the owning conversation. `/agents` supports catalog selection, but that is not a live-child control. [Panel model](../../crates/heycode-tui/src/side_panel.rs#L1).

16. **P2 — Child streaming events are suppressed rather than routed (Source).** Native children use a quiet EventBus. Durable logs exist, but reasoning/text/tool deltas do not feed an owner-aware child view. Route events using child IDs; keep the parent transcript uncluttered while making the child observable. [Construction](../../crates/heycode-agent/src/subagent.rs#L166).

17. **P2 — No per-child task telemetry view (Source).** Current task snapshots expose identity/label/state, not elapsed time, current tool, model, token usage, cost, output volume or workspace changes. Add these to a stable task projection instead of inferring progress from text. [JobSnapshot](../../crates/heycode-agent/src/jobs.rs#L125).

18. **P2 — Concurrent tool display is not a live execution timeline (Source + Test).** Calls are announced and results committed in model order, even when execution overlaps. The existing test intentionally verifies one visible call at a time. Preserve protocol result ordering while emitting separate actual-start/progress/finish events. [Scheduler explanation](../../crates/heycode-agent/src/schedule.rs#L43).

19. **P2 — Background shell output is completion-only and truncated (Source).** The completion notice carries a bounded excerpt (about 6 KiB), not a durable output handle that the model/UI can tail and page through. Retain stdout/stderr with offsets, truncation metadata and retrieval after completion. [Output notice](../../crates/heycode-agent/src/execution_jobs.rs#L528).

20. **P2 — Background PTY completion does not include process output (Source).** It reports terminal exit status; `/ps` lists status and buffer counts. Even when terminal tools are enabled, output inspection requires a separate lookup and drain. Return job and terminal identity together, expose output in the task view, and preserve a readable completion tail. [PTY start](../../crates/heycode-agent/src/execution_jobs.rs#L110), [notice](../../crates/heycode-agent/src/execution_jobs.rs#L518).

21. **P1 — Monitor tool is missing (Source + live inventory).** A long-lived `tail -f` launched with `background_shell` will not deliver matching lines while it continues running. PTY polling is possible when enabled, but there is no monitor abstraction for filters, event delivery, deduplication, batching, rate limits, lifecycle and cancellation. Add command-output monitoring first; optionally support WebSocket events through the same delivery contract.

22. **P2 — No first-class targeted watch configuration (Source).** Schedules can periodically prompt the agent, but they do not bind a filter to an existing process/output stream. Required examples: notify only on `ERROR`, stop after a readiness marker, watch one CI run, debounce file changes, and cap event-driven model wakeups. This is an extension of the missing Monitor capability, not a claim that scheduled polling is impossible.

### Custom agents, workflows and teams

23. **P2 — Custom agent schema is minimal (Source).** `AgentDocument` allows only `display`, `instructions`, `provider`, `mode`; unknown fields are rejected. Missing preset options include model/effort, tool allow/deny lists, scoped skills/MCP, permission mode, maximum turns, persistent memory, isolation and default background behavior. Implement only settings whose runtime semantics are enforced. [Schema](../../crates/heycode-extension-host/src/lib.rs#L1048).

24. **P2 — Preset instructions are appended to the user prompt (Source).** The task tool wraps them in `<agent-preset>` text; they are not a distinct child developer/system instruction layer. Introduce a dedicated instruction layer so role policy and the task request are separately represented. [Assembly](../../crates/heycode-agent/src/subagent.rs#L512).

25. **P3 — No custom-agent authoring/reload workflow (Source).** Files load during composition; the catalog does not create/edit/validate/reload a preset. Invalid files become skipped declarations. Provide visible diagnostics, reload and a preview of the resolved configuration. [Loader](../../crates/heycode-extension-host/src/user_declarations.rs#L66).

26. **P3 — No Claude/Codex agent-file import (Source).** dshx consumes its JSON schema, not `.claude/agents/*.md` or `.codex/agents/*.toml`. An importer must flag unsupported settings rather than silently discard policy. There is also stale source commentary saying Codex has neither hooks nor agents; at least the agents claim is contradicted by current official documentation. [Comment](../../crates/heycode-extension-host/src/user_declarations.rs#L14).

27. **P2 — No fork-and-continue combination in the task interface (Source).** `fork` means inherited context plus one-shot; `continuable` means fresh context. Expose seed and continuation independently when the provider supports the combination. [Modes](../../crates/heycode-agent/src/subagent.rs#L453).

28. **P2 — Child follow-up blocks the calling tool turn (Source).** `send_message` awaits the whole child reply; no background follow-up option or nonblocking steer/wait pair exists in that tool. Team dispatch can run a follow-up in a job, but is a heavier path. Add nonblocking message IDs and bounded wait-for-change. [Send](../../crates/heycode-agent/src/subagent.rs#L635).

29. **P2 — No model-facing close/archive child control (Source).** Registry lifecycle APIs exist, but the exposed four-tool set has no explicit close operation or completed-child restore operation. Interrupting a turn is not closing a retained conversation.

30. **P1 — Default workflow is not an agent/tool workflow executor (Source).** The shipped `SequentialWorkflowWorker` supports only Progress and Delay capabilities and executes `Emit` or `Delay`. There is no built-in agent step, tool step, result binding, fan-out/fan-in, conditional branch or bounded retry. Durable workflow bookkeeping is real; general agent execution behind it is absent. [Worker](../../crates/heycode-agent/src/workflow.rs#L124).

31. **P2 — Workflow tool schema does not teach the definition shape (Source).** The top-level tool takes a generic definition object. A model needs the supported action schema, required capabilities, identifiers, limits and examples. Publish the actual nested schema and fail with repairable validation errors. [Tool](../../crates/heycode-agent/src/workflow.rs#L641).

32. **P2 — Team creation does not launch a ready team (Source).** A lead creates a roster, starts continuable children, waits for initial handles, adds members, creates tasks and dispatches them. There is no atomic role-based spawn/bootstrap or automatic ready-task dispatcher. The existing task dependency and revision machinery should be reused. [Member registration](../../crates/heycode-agent/src/team.rs#L202), [dispatch](../../crates/heycode-agent/src/team.rs#L352).

33. **P2 — Team mail is a durable mailbox, not automatic conversation delivery (Source).** `send_mail` commits a record; recipients use mailbox/claim actions. There is no push into the recipient's active conversation in that method. Add owner-aware delivery and an explicit delivered/claimed state while preserving access checks. [Mail](../../crates/heycode-agent/src/team.rs#L276).

34. **P2 — No team/task dependency UI (Source).** Users cannot inspect members, dependencies, blockers, mail or worktree results in an integrated view. Revision conflicts are also left to model/tool orchestration. Add a task board backed by the existing durable projection.

35. **P2 — External runtime capability parity is limited (Live catalog + Source).** Native advertises continuation/fork, while default Codex/Claude delegated providers advertise neither. Optional worktree providers accept fresh one-shot requests only; native worktree execution is absent from that composition. Show readiness and supported modes per provider, and test installed runtimes separately. Do not infer capabilities from the external tool's brand name. [Worktree constraints](../../crates/heycode-agent/src/worktree_subagent.rs#L213).

36. **P2 — Parent configuration inheritance needs explicit semantics (Source, validation needed).** Native children share tools/pre-tool guards, but the runner captures `LlmSelection` at construction and creates a new Agent with its own turn machinery. Per-agent overrides are absent, and parent model changes should be regression-tested to establish whether newly spawned children use the intended selection. Do not describe all parent settings as inherited until that is verified. [Runner construction](../../crates/heycode-agent/src/subagent.rs#L820).

37. **P1 — Plan mode lacks the complete review-and-permission handoff (Source; user-requested gap).** dshx already has a durable read-only plan state, a mutation guard, `/plan`, and `exit_plan_mode(plan)` accepting a complete Markdown plan. However, the permission picker lists Full access, Accepted edits and Default without Plan, and the exit tool reduces review to a generic Allow/Deny decision. On Allow it switches plan mode off; it does not itself select Accepted edits or Default. The requested integrated flow is therefore incomplete, rather than plan enforcement being entirely absent. [Plan guard](../../crates/heycode-agent/src/plan.rs#L208), [exit tool](../../crates/heycode-agent/src/plan.rs#L277), [permission choices](../../crates/heycode-tui/src/permission_picker.rs#L24).

    **Required behavior:** expose Plan in permission/mode switching. While selected, the agent may inspect and reason but must not edit project files or perform mutating tool actions, including through subagents or background work. At the end of planning, it must call `exit_plan_mode` with the full detailed plan/report: objective, proposed changes, affected areas, implementation steps, assumptions, risks and validation. Display the complete document in a readable, scrollable review view, followed by these explicit choices:

    | Review choice | Resulting state | Next action |
    |---|---|---|
    | **Yes, accept the plan and make changes** | Leave Plan; select **Accepted edits** permissions | Continue implementation under that policy. This does not grant Full access. |
    | **Yes, use Default permissions** | Leave Plan; select **Default** permissions | Continue implementation and ask for permission for actions requiring approval under Default. |
    | **No, stay in Plan mode** | Remain in **Plan** with mutation blocking active | Let the user provide feedback, revise the plan, and present it for review again. |

    This needs a dedicated plan-review result carrying the chosen target permission mode, not a generic tool approval boolean. Commit the chosen permission policy and plan exit together before implementation can start; if either transition fails, keep edits blocked. Rejection, Escape or dismissal must not authorize implementation. Keep the proposed plan and feedback available across revisions and resume. Automatic tool-approval policies must not silently substitute for this explicit plan decision. Entering Plan while work is active also needs a defined safe boundary: settle or cancel mutating background work before reporting Plan as fully active.

    **Acceptance tests:** enter Plan through mode switching; attempt direct edits, shell mutations and delegated/background mutations and verify blocking; submit a long detailed plan and verify full review rendering; exercise each of the three choices and assert the actual permission policy before the next tool runs; verify feedback/revision and Escape remain read-only; resume pending/rejected/accepted review states; inject a failed permission transition and ensure no edit starts. Test policy state and real tool behavior, not just the footer label.

## Current competitor reference points

These are specific documented comparisons, not an assertion that every feature is available on every plan, provider or version.

- **Codex:** official documentation describes custom TOML agent definitions, agent configuration overrides and live subagent navigation. Its CLI offers `/agent`; its IDE presents expandable child activity. dshx's provider catalog does not supply that experience. [Official subagents documentation](https://developers.openai.com/codex/subagents).
- **Claude custom agents:** its documented subagent configuration supports substantially richer model, tool, permission, skill, memory and isolation choices than dshx's four-field JSON schema. [Official subagent documentation](https://code.claude.com/docs/en/sub-agents).
- **Claude background/Monitor:** its tools documentation describes background Bash handling and Monitor delivering command output lines or WebSocket messages while a conversation continues. Monitor has provider/environment availability restrictions. dshx completion notices are a different, narrower mechanism. [Official tools reference](https://code.claude.com/docs/en/tools-reference).
- **Claude teams:** documented experimental teams combine shared task dependencies and inter-agent communication with teammate navigation; they also have documented limitations. dshx already has useful task/mail data structures but lacks comparable interactive coordination. [Official teams documentation](https://code.claude.com/docs/en/agent-teams).
- **Claude detached agents:** the separate agent-view research preview describes detached sessions and supervisor interaction. dshx's process-owned background jobs should not be described as equivalent unattended sessions. [Official agent-view documentation](https://code.claude.com/docs/en/agent-view).

The exact blue treatment described by the user is a requested UX target; this audit did not run Claude Code to verify its pixels or current shortcut behavior.

## What was tested

1. `cargo test -p dshx-agent --test main`: **201 passed**. Includes native/delegated fixture subagents, background jobs, shell/PTY ownership, workflows, teams, schedules and parallel tools.
2. `cargo test -p dshx-extension-host`: **26 passed** across its unit/integration targets, including declarations and extension behavior; zero failures.
3. Isolated `doctor --composition`: healthy composition/activation. This checks wiring, not external executable/auth readiness.
4. **Real PTY plus local OpenRouter-compatible HTTP fixture**, using the debug binary and fresh temporary home/workspace: normal onboarding, tool approval, background native continuable child, immediate parent reply, `/tasks`, `/agents`, and `/stop`. Verified the advertised tool payload, job presence, missing early child count and delayed cancellation settlement. No paid provider or external agent runtime was invoked.

The existing background cancellation test uses a custom fixture provider that cooperates with its token. It therefore passes while the native first-turn path misses cancellation. This is the most important test-coverage lesson from the audit. [Existing test](../../crates/heycode-agent/tests/it/subagent.rs#L348).

Initial local harness attempts needed corrections for onboarding config, GLM-required reasoning fields, approval handling and the zero-based job ID; those harness errors are not product findings. The final cancellation capture used `/stop job-0` after closing the catalog.

Evidence captured locally: `/tmp/dshx-multiagent-audit/` contains composition output, request payloads, fixture script, test logs and terminal captures. Screenshots below were copied into the repository for durable review. They were rendered from real PTY output with the harness's no-color setting; they establish structure, not theme colors.

Running background child (local-only evidence: `docs/audits/assets/multiagent-2026-09-07/running-jobs.png`)

Agents catalog while child is running (local-only evidence: `docs/audits/assets/multiagent-2026-09-07/agents-catalog.png`)

## Acceptance suite to build

Use deterministic local provider fixtures for lifecycle assertions, actual subprocesses for execution, and real PTYs for interaction. Add separate optional smoke tests for installed/authenticated external runtimes; do not make paid calls the basic correctness test.

| Scenario | Required pass condition |
|---|---|
| Plan review and permissions | Plan blocks mutations; full plan is reviewable; the three choices select Accepted edits, Default or continued Plan exactly; feedback and resume preserve the decision. |
| Three background children | All receive stable IDs before response; all run concurrently; parent stays usable; each settles once. |
| Native cancellation | Cancel during first provider stream, first tool, follow-up and nested child; bounded stop; no later side effect after cancellation acknowledgement. |
| Running child navigation | Bottom row visible; keyboard/click opens child; live tool/text updates; steering reaches selected child; return preserves parent draft. |
| Child approval | Approval identifies owner and tool; open the owner; accept/reject once; concurrent requests remain correctly routed. |
| Background shell | Print start, delay, print end; output is visible before exit; final status/output retrievable afterwards. |
| Background PTY | Start server, obtain both IDs, see readiness, send input, resize, stop; defaults expose every required tool. |
| Output bounds | Large stdout/stderr, invalid bytes and terminal controls do not break UI; truncation is explicit and retained output remains pageable. |
| Monitor | Script emits irrelevant lines plus two matching changes while remaining alive; only matching events wake agent; cancellation ends watcher. |
| Monitor storm | Flood events; bounded queue, debounce, dedupe and spend/wake caps; no runaway model loop. |
| Custom agent configuration | User/project precedence, invalid config diagnostics, reload, model/tool policy enforced in the actual child request. |
| Parent model switch | Switch model, spawn child, inspect provider payload; behavior matches declared inheritance policy. |
| Agent workflow | Research and code agents fan out, review waits on both, failure/retry is bounded; restart does not repeat completed side effects. |
| Teams | Bootstrap members, dispatch dependency chain, deliver mail during work, reject wrong-owner access, display blocker and result. |
| Worktree edits | Child edits tracked/untracked files; finish/cancel/error all preserve a reviewable artifact; cleanup occurs only after preservation. |
| Restart | Kill app mid-job; reopen shows interrupted state; explicit safe retry; no false running claims or duplicate completion notices. |
| Scale | Many completed/running jobs; bounded retention/rendering; queue and cost limits are visible. |

## Implementation order

1. Fix native cancellation and stop feedback; reconcile PTY defaults; guarantee worktree result preservation; complete the explicit Plan review and permission handoff.
2. Introduce one observable task record for jobs, child sessions and terminal executions, with stable IDs at admission, parent links, timestamps, lifecycle, output handles and owner-aware events.
3. Build the bottom task strip, expandable job details and live child switcher on that record.
4. Add retained streaming output and Monitor with filtered/bounded event delivery.
5. Expand custom-agent configuration and make inheritance/policy enforceable.
6. Implement actual agent/tool workflow steps and team bootstrap/delivery/UI using the existing durable projections.
7. Add detached execution only with explicit restart, ownership and budget semantics.

Do not rebuild the existing job registry, durable inbox, session storage or team dependency model from scratch. The missing work is to connect and extend them into a coherent runtime and interface, then verify the exact user journeys above.
