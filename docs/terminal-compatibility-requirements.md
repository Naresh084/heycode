# heycode TUI parity — Phase 2 audit and implementation plan

Date: 10 September 2026. Status: **audit complete; implementation and validation active**.

## Decision

Phase 1 did not establish end-to-end product parity. The user’s live retest exposes a broken file-discovery path, misleading reasoning controls, and an execution console presented as though it were an agent navigator. The next phase must fix those foundations and introduce a proper work-item domain before expanding the tool catalog. Passing unit tests alone is not acceptance.

The requested outcome is a coherent flow: discover files reliably → create planned work → launch and navigate multiple agents → coordinate a team when requested → inspect results → see accurate, explained context usage. Agents, work items, processes, and teams need separate identities, counts, and controls.

## Evidence and limits

- Inspected all three supplied screenshots and the corresponding native heycode session, including its nine request records, tool calls, usage, and reasoning events. No credentials or opaque provider payloads are included here.
- Ran a temporary read-only Rust probe through the real composition harness: **48 registered tools and 64 slash commands**. Registered does not mean configured, advertised to the model, or successfully executed.
- The actual user session advertised **47 custom tools** and selected `openrouter:web_search` through `native_tool_routes` with native feature `web`. The missing custom-function entry is intentional native routing, not evidence of unavailability. Live search execution still needs validation.
- Reproduced the repository glob failure and isolated a two-file reproduction using the public filesystem service. No product implementation was changed for this report.
- Installed Claude Code reference: **2.1.267**. Inspected its initialization command inventory and captured its current agent footer. The initialization list includes injected skills and is not an exhaustive interactive command catalog.
- The Claude comparison uses the current official [tools reference](https://code.claude.com/docs/en/tools-reference), [commands reference](https://code.claude.com/docs/en/commands), [subagent documentation](https://code.claude.com/docs/en/sub-agents), and [team documentation](https://code.claude.com/docs/en/agent-teams). Availability varies by version, model, platform, authentication, and feature flags. The reference screenshot establishes a navigation pattern, not a fresh full Claude team lifecycle test.
- This is an audit and implementation plan. Provider-wide correctness, every integration, live team recovery, and every keyboard transition have not been certified.

## Screens and flow health

### 1. Launch and return from five agents — partial

User retest: five completed agents and a mixed Tasks footer (local-only evidence: `docs/audits/assets/terminal-comparison-2026-09-10/01-user-retest.png`)

The inline five-agent tree is real progress. However, the bottom entry says `Tasks 9` after five agents, with no active work. The `TaskKind` domain explicitly includes `Child`, `Job`, `Tool`, and `Team`; `RegistryTaskSource` feeds these into the same console. The session ran five `task` calls and four Bash calls, consistent with the nine-count symptom. The screenshot alone does not prove each underlying row’s identity, so the implementation must assert that mapping directly.

**Failure:** the user cannot tell whether Tasks means planned work, conversations, shell executions, or teams. The inline tree’s conversation affordance and footer’s Tasks affordance lead into the same broad surface. A completed one-shot result also needs a distinct treatment from an idle, resumable agent.

**Target:** an agent entry next to the composer with active/waiting counts and a direct navigator. Preserve the inline tree and its stable links. Keep process history in a Jobs view. Put structured work in a Work view. Keep a combined diagnostic inventory as an optional view, not the primary agent interaction.

### 2. File discovery — broken, reproduced

User retest: glob failure and fallback (local-only evidence: `docs/audits/assets/terminal-comparison-2026-09-10/02-user-retest.png`)

`glob("**/*")` fails against this repository. The public filesystem service returns `InvalidOutput`; the tool collapses it into the unhelpful `filesystem operation failed` message.

**Confirmed cause:** the local backend sorts `PathBuf` values by path components. The service validates the resulting strings using lexical string order. These orders differ. `a/x` sorts before `a-b` as a path, but after it as a string. A directory containing just these two files reproduces `InvalidOutput`.

The real repository contains the same mismatch: `crates/heycode-agent/src/agent/native_inspection.rs` precedes `crates/heycode-agent/src/agent.rs` in component order, violating the string validator. Relevant code: `heycode-exec/src/filesystem/local.rs`, `collect_files` and `glob`; `filesystem/service.rs`, `valid_glob_output`.

| Probe root | Result |
|---|---|
| Repository root | InvalidOutput |
| `crates` | InvalidOutput |
| `tmp` | InvalidOutput |
| `crates/heycode-tools` | Success, 39 matches |
| `docs` | Success, 206 total matches with bounded display |
| Two-file `a/x`, `a-b` fixture | InvalidOutput |

**Fix design:** normalize and order by the published result representation before applying the result cap; preserve uniqueness and service validation. Do not simply sort an already-truncated subset. Apply the same contract check to Grep, which shares traversal and ordering assumptions. Preserve capability-root boundaries. Then address cancellation, unreadable descendants, explicit partial results, Unicode/non-UTF8 path policy, ignored directories, and bounded traversal separately. The current matcher supports `*`, `?`, and `**`; richer syntax requires an explicit contract, not accidental promises. Root-level failure must include a useful error code and safe diagnostic detail.

### 3. Reasoning expansion — broken, exact event path identified

Expanded empty thought (local-only evidence: `docs/audits/assets/terminal-comparison-2026-09-10/03-user-empty-thought.png`)

All **nine** recorded reasoning chunks in this session were empty strings; readable reasoning characters: **zero**. The provider returned encrypted reasoning details. `heycode-llm/src/chat.rs` deliberately emits an empty `ReasoningDelta` to signal that activity. `heycode-tui/src/app.rs` unconditionally creates an expandable reasoning item, and elapsed time rounds down to `0s`.

This does **not** prove the model did no internal reasoning. It proves that heycode has no readable reasoning to expand and displays a misleading control.

**Fix design:** separate activity from displayable content. Use explicit states for no reasoning, activity without readable content, readable content, completion, and interruption. Only readable non-whitespace text creates an expandable block. Opaque reasoning stays in the provider continuation state; never show ciphertext or invent explanatory reasoning. When only activity is known, use a transient activity indicator or a noninteractive summary. Do not claim a measured thinking duration from first observed packet to completion. For genuinely measured short UI activity use `<1s`, not `0s`.

Test streaming and resumed history: empty deltas, whitespace, encrypted-only details, readable summaries, mixed text/encrypted details, multiple reasoning segments, cancellation, error, keyboard expansion, and mouse hit regions. Empty historical rows must also disappear or become noninteractive on replay.

### 4. Context growth — real growth plus imperfect estimation, not a demonstrated percentage bug

The active model record uses a **1,048,576-token window**. The latest stored pre-request estimate was **19,693 tokens**. The provider reported **18,954 input tokens** on that request. A displayed value near **1.9%** is consistent with these numbers; the screenshot does not show a runaway percentage calculation.

| Request | Estimated input | Provider input |
|---|---:|---:|
| Initial greeting | 7,275 | 9,372 |
| Start five-file request | 7,799 | 9,603 |
| After failed glob | 9,338 | 10,081 |
| Discovery fallback | 10,684 | 11,808 |
| Further shell fallback | 12,364 | 13,330 |
| Further shell fallback | 12,957 | 13,623 |
| Dispatch agents | 13,546 | 13,935 |
| Consolidate results | 18,634 | 18,203 |
| Follow-up question | 19,693 | 18,954 |

The nine request headers each contain approximately **31,528 serialized characters of tool definitions** (Python JSON serialization), with 47 tools exposed. This is a size measurement, not an exact tokenizer count. The same session required a failed glob plus four shell calls before dispatch. Agent results then increased the parent prompt. These are concrete contributors to growth. There is also estimator error: the first estimate understates provider input by 2,097 tokens, while the last overstates it by 739.

`context_budget()` uses the latest request envelope, not a lifetime sum. After a response, `record_context_growth()` adds completion usage and tool-result estimates and marks the budget projected. This deserves validation because generated-token usage, hidden reasoning, and retained next-request bytes need not be identical. Each next request recomputes its envelope. No confirmed child-token double-counting was established by this audit.

**Fix design:**

1. Scope measurements to `(session, request, provider, model)` and distinguish latest request, projected next request, and lifetime usage.
2. Show a compact line such as `Context ~19.7k / 1.05M · 1.9% used`; show output reserve and the 80% compaction threshold in details. Keep input/output billing usage separately labeled.
3. `/context` must break down system instructions, guidance/skills, advertised tool schemas, conversation text, tool results, attachments, and opaque continuation data. Attribute each byte/token once. Display confidence and measurement time.
4. Reconcile estimates against provider usage where semantics are compatible; preserve cached-input accounting and provider-specific differences. Do not replace next-request state with an unrelated cumulative billing total.
5. Audit projection for duplicate event delivery, child event leakage, retries, fallback, cancellation, model switching, and compaction. Persist before/after measurements.
6. Reduce overhead by fixing discovery retries, returning bounded agent summaries with references to full output, and stabilizing the request prefix. **Tool-search expansion is deferred by the latest user instruction.** heycode already has a deferred-tool mechanism; retain its inventory status without expanding it in this phase. Do not remove core tools simply to make the percentage smaller.
7. Add a repeatable benchmark with no tools, one file read, five agents, a long output, a retry, and compaction. Record estimates, actual usage, parent-only growth, and schema size. Define a target after a baseline rather than inventing a universal accuracy guarantee.

### 5. Visual hierarchy — needs redesign within existing theme system

The screenshot applies orange/red to headings, inline code, assistant markers, and the persistent Tasks footer. The current dark theme accent is `#D97757`; error is `#E06666`. Overusing the accent weakens hierarchy and makes ordinary content resemble a warning. Literal Markdown heading markers also remain visible in the screenshot; include heading rendering in the visual pass.

Proposed semantic roles, implemented through existing theme tokens rather than scattered renderer constants:

| Role | Candidate dark color | Treatment |
|---|---|---|
| Main text | `#E8EAF0` | Normal body and completed results |
| Secondary text | `#A6ADBB` | Metadata, elapsed time, counts |
| Focus / active navigation | `#7CB7FF` | Selected agent and active control |
| Code / references | `#C6A0F6` | Restrained inline-code foreground |
| Success | `#8BD5A0` | Confirmed completion marker |
| Waiting / attention | `#EBCB8B` | Approval or input needed |
| Failure | `#F28B82` | Actual error only |
| Background | `#181B22` | Candidate dark surface |

These are proposed tokens, not visually accepted final colors. Validate foreground contrast against actual terminal backgrounds, light theme, ANSI palettes, truecolor and 256-color fallback, color-vision differences, and no-color mode. Every status also has a label or distinct glyph. Keep full-width saturated footers out of the normal completed state.

**Latest layout direction:** retain only the heycode banner as a visual difference. Below it, match the live Claude transcript, working indicator, queued-message view, composer, status and permissions footer, Background browser, shell inspector, agent inspector and foreground-agent switcher. The Background panel replaces the composer and footer while retaining the transcript above it. Its grouped list opens one inspector; Left returns to the list, Escape closes the panel, and `f` foregrounds a conversation. A foreground conversation uses its own transcript and composer, with a vertical main/agent switcher underneath. Shell output belongs only to the selected shell’s detail box. This user direction supersedes all earlier custom footer layouts.

### Claude navigation reference

Claude current agent footer (local-only evidence: `docs/audits/assets/terminal-comparison-2026-09-10/04-claude-agent-footer.png`)

Claude itself still exposes background agents through `/tasks`; its agent footer and agent view provide the missing direct navigation layer. Therefore the heycode correction is not to ban agents from every task inventory. It is to stop making a mixed execution inventory the only prominent route into conversations. See the official [commands reference](https://code.claude.com/docs/en/commands) and [subagent guide](https://code.claude.com/docs/en/sub-agents).

## Proposed end-to-end interaction

1. **User asks for five file checks.** Main conversation acknowledges the scope. Glob returns reliable, bounded source paths. No task or agent is created merely because a discovery tool ran.
2. **Create planned work if useful.** `task_create` produces stable work-item IDs with subject, description, status, owner, dependencies, metadata, and revision. Five independent items may all be in progress; remove TodoWrite’s single-in-progress constraint.
3. **Launch agents.** A model-facing `agent` surface creates named child conversations using the existing native runtime. The old `task` spawn name becomes a compatibility alias, not a second domain. A spawn result returns agent identity, lifecycle, work-item links, and output references.
4. **Show live work.** The parent transcript contains one expandable launch group with five stable children, individual progress, and errors. The footer exposes agent names directly, with an Agents selector for the full list. Background shells/jobs open from the permissions row; optional Work entries expose structured tasks.
5. **Navigate.** Click an agent or use the mapped agent shortcut to open its conversation. Preserve the parent draft, selection, scroll, and output cursor. Parent and child have distinct context meters. Returning restores the exact parent state.
6. **Steer and stop.** A message targets an explicit agent ID. A stopped one-shot agent cannot silently accept a message as though it were live; offer an explicit resume where supported. Cancelling first shows cancelling, then a settled state. Never stop a shell job merely because its number matches an agent ID.
7. **Coordinate a team when requested.** Create a team, add named members, expose shared work, assign or claim ready items, enforce dependencies and ownership, route mail, and show waiting reasons. Team creation must make its default member creation clear.
8. **Settle.** Results return once to the parent as bounded summaries. Full child transcripts remain navigable. Agent completion and work-item completion are independent until the outcome has been checked. Failed work does not turn green because the child exited normally.
9. **Finish.** The parent provides its consolidated response. Active agent counts drop; completed history stays discoverable without an alarming Tasks banner. Unread results remain indicated until seen.
10. **Resume.** Replay reconstructs work, teams, agents, and jobs from durable state. A restarted process does not resurrect dead workers as running. Recovery is explicit, and repeated events do not duplicate work or token growth.

### Domain and API boundaries

| Domain | Identity and storage | Proposed surface | UI owner |
|---|---|---|---|
| Work item | WorkItemId, durable revisions | `task_create/get/list/update` | Work board/list |
| Agent conversation | Existing child/session identity | `agent`, `send_message`, `list_agents`, stop/resume | Agent navigator and inline tree |
| Process/tool execution | Existing job/call identity | Existing execution tools | Jobs console and tool cards |
| Team | TeamId, members, work links, mailbox | Existing team service extended | Team overview |
| Workflow | Workflow/run/phase identity | Existing workflow runtime | Workflow console |

Keep wire names in the repository’s snake_case convention; map Claude names by behavior. A rename alone is not feature parity. Share work-item storage with teams rather than creating another independent checklist. Existing team tasks need a migration/adapter path preserving IDs and revisions. Existing TodoWrite sessions should replay read-only and migrate once into work items; remove the old tool from new model requests, update prompts/help/tool search/permissions, and prevent new whole-list overwrites from destroying concurrent updates.

The structured task contract needs create/get/list/update, dependency cycle detection, blocked-by and blocks relationships, assignment, deletion/tombstones, paging, optimistic concurrency, idempotency, and session/team scope. Listing work must never launch an agent. Completing work must never implicitly kill one.

### Team gaps requiring completion

The current `team` tool is substantive: create, snapshot, add_member, create_task, dispatch_task, send_mail, mailbox, claim_mail, wait, recover, dispatch_ready, and deliver_mail. It is **not missing wholesale**. However, its declared action surface does not provide a complete member-removal, shutdown, team-delete/archive, or general task-update/reassignment flow. It also bootstraps worker/reviewer roles by default and caps initial roles at eight.

Phase 2 should provide explicit roster management, shared work updates, owner/lead checks, reliable mail delivery with deduplication, orderly worker shutdown, recoverable team archival, and restart recovery. Avoid destructive cleanup while members are active. Do not implement an arbitrary one-agent/five-agent UI ceiling; use scrolling/virtualization and configurable runtime resource limits. The existing eight-role bootstrap cap should be reviewed explicitly rather than described as unlimited.

## Tool inventory and parity

`Present` means a registered implementation was found; it is not a claim of full live equivalence. `Partial` means a related surface exists but semantics, availability, or coverage differ. `Missing` means no equivalent native model-facing surface appears in the inspected composition. `Integration` marks infrastructure-specific features that need a deliberate heycode counterpart. Claude task tools are model-gated; recent Claude versions omit the structured task tools on some newer models unless enabled. Our decision to adopt structured work is a heycode product choice, not a claim that every current Claude session exposes it.

| Claude reference tool | heycode status | Existing surface | Phase 2 action |
|---|---|---|---|
| Agent | Partial | task | Rename primary spawn surface; finish direct agent navigation and resume semantics. |
| Artifact | Partial | artifact | Validate local artifact workflow; hosted publishing is a separate integration. |
| AskUserQuestion | Present | ask_user_question; ask_user_question_async | Verify pending, answered, cancelled and resumed states. |
| Bash | Present | bash; background_shell; terminal_* | Audit foreground/background handoff, retained output and interruption. |
| CronCreate | Partial | schedule_create | Verify recurrence, restart and session ownership semantics. |
| CronDelete | Partial | schedule_delete | Verify cancellation of already-admitted wakeups. |
| CronList | Partial | schedule_list | Expose next run, timezone, state and owner. |
| Edit | Present | edit | Retain read-before-write and precise failure behavior. |
| EndConversation | Missing | — | Low priority; explicit session-end contract if adopted. |
| EnterPlanMode | Missing | /plan is human-facing | Add model-facing entry with durable mode transition. |
| EnterWorktree | Partial | worktree runtime/subagent infrastructure | Add explicit current-session entry, scoped roots and recovery. |
| ExitPlanMode | Present | exit_plan_mode | Verify plan presentation and approval transition. |
| ExitWorktree | Partial | worktree infrastructure | Add safe current-session return without implicit deletion. |
| Glob | Broken | glob | Fix published ordering contract; expand traversal acceptance. |
| Grep | Partial | grep | Check shared ordering issue; richer context/output/regex options need contracts. |
| ListAgents | Partial | list_tasks | Agent-only inventory; current scope differs from cross-session messaging. |
| ListMcpResourcesTool | Partial | MCP resource registry | Expose model-facing resource listing; not in default tool catalog. |
| LSP | Partial | lsp_servers; lsp_definition; lsp_references; lsp_diagnostics | Consolidate discovery; audit additional language-server operations. |
| Monitor | Partial | monitor | Verify event bounds, cancellation, and supported source types. |
| NotebookEdit | Present | notebook_edit | Exercise actual notebook cell edit/insert/delete behavior. |
| PowerShell | Missing | generic shell execution only | Native platform adapter if Windows becomes supported. |
| PushNotification | Integration | — | Local notification first; phone delivery requires a service. |
| Read | Partial | read; notebook_read; attachment/document paths | Add ranged/paged model reads and unify rich-file discoverability. |
| ReadMcpResourceTool | Partial | MCP resources infrastructure | Expose scoped resource reads to model tooling. |
| RemoteTrigger | Integration | local schedules are different | Requires hosted routine execution, authentication and lifecycle. |
| ReportFindings | Missing | native review output exists | Typed findings with locations and failure scenarios; UI consumes structured data. |
| ScheduleWakeup | Partial | schedule_create | Add explicit self-paced reschedule/stop behavior if adopted. |
| SendFeedback | Missing | — | Local reviewable draft queue; sending remains explicit. |
| SendMessage | Partial | send_message; team mail | Unify targeting while preserving authority; cross-session transport is separate. |
| SendUserFile | Integration | artifact/attachments are adjacent | Add local delivery card; remote transport requires a client service. |
| ShareOnboardingGuide | Integration | — | Optional hosted sharing feature, not an agent-runtime blocker. |
| Skill | Present | load_skill | Retain independent skill discovery and loading. |
| TaskCreate | Missing | todo_write; team create_task are partial predecessors | New general structured work-item creation. |
| TaskGet | Missing | team snapshot is adjacent | Retrieve one work item by stable identity. |
| TaskList | Missing | list_tasks currently lists agents | Work-item inventory distinct from agent inventory. |
| TaskOutput | Partial | job_output; child transcript/output | Keep paged output references; do not prioritize a deprecated name. |
| TaskStop | Partial | interrupt_task; cancel_job; terminal_kill | Typed target dispatch with settlement and useful not-found results. |
| TaskUpdate | Missing | todo_write replaces whole list | Revision-safe status, details, dependencies, assignment and deletion. |
| TodoWrite | Replace | todo_write | Remove from new requests after migration to structured tasks. |
| ToolSearch | Present | tool_search | Existing capability; expansion deferred. Prioritize stable definitions and cache telemetry now. |
| WaitForMcpServers | Partial | MCP connection lifecycle | Add model-visible bounded readiness wait or include it in tool search. |
| WebFetch | Present | web_fetch | Validate configured transport and capped readable output. |
| WebSearch | Native route selected in retest | web_search → openrouter:web_search | Verify native execution/citations and complete provider capability gating. |
| Workflow | Present | workflow; run_code | Keep workflow opt-in and separate workflow/agent/work identities. |
| Write | Present | write | Retain safe overwrite and path policy. |

Experimental team-specific surfaces such as `TeamCreate`/`TeamDelete` belong in a gated team profile rather than the common catalog above. The official team guide establishes the experimental team feature; these exact names are not in the current common tools table and were not freshly enumerated under a team-enabled live request in this audit. Implement their create/cleanup behavior through heycode’s team service without claiming a verified always-on Claude schema.

### All registered heycode tool names

- `read`, `write`, `edit`, `bash`, `glob`, `grep`, `todo_write`, `web_fetch`
- `web_search`, `notebook_read`, `notebook_edit`, `artifact`, `transcribe_audio`, `computer`, `browser`, `lsp_servers`
- `lsp_definition`, `lsp_references`, `lsp_diagnostics`, `load_skill`, `task`, `send_message`, `list_tasks`, `interrupt_task`
- `exit_plan_mode`, `ask_user_question`, `ask_user_question_async`, `list_jobs`, `cancel_job`, `terminal_open`, `terminal_write`, `terminal_read`
- `terminal_resize`, `terminal_kill`, `terminal_list`, `background_shell`, `background_terminal`, `job_output`, `monitor`, `run_tool`
- `goal`, `workflow`, `schedule_create`, `schedule_list`, `schedule_delete`, `team`, `run_code`, `tool_search`

Useful heycode-specific surfaces include audio transcription, browser/computer integration, persistent terminals, code execution, goals, asynchronous questions, and explicit background jobs. Keep them where they work. Their existence does not compensate for a broken Glob or undiscoverable core capability.

### Availability contract

For every tool record: owner → service registered → prerequisite configured → route compatible → permission eligible → schema advertised or deferred → successfully exercised. `/tools` or an equivalent inspector should show these stages and an unavailable reason. Count enabled tools separately from registered tools. Tests should inspect the actual serialized provider request, not just `ToolRegistry::specs()`.

**Implementation progress:** `/tools [name] [json]` now provides a read-only inventory with exact plugin attribution, canonical names and aliases, setup metadata, current native route selection, permission-policy limitations, the latest matching prepared request, and correlated successful client/native calls. It starts no inference or tool execution. File and shell service binding, browser installation, transcription command, OS requirements and language-server configuration have separate setup descriptions. A successful `status` call records that action explicitly and does not certify the main operation. Unknown enabled counts remain unknown where configuration or permission depends on arguments. Prepared request metadata is historical evidence; wire serialization, the next request, and remote provider eligibility remain separate checks. The interactive inspector and all applicable Claude tool/command UI states remain required under P2-UI03.

## Native slash-command inventory

The following is the current public native-command set from the official reference, with aliases folded into their primary row. Native commands that manage skills/plugins remain included; individual skills, plugin commands, MCP prompts and bundled skill invocations are excluded. The heycode column is a composition/name mapping, **not a full behavior certification**. All heycode observations come from the 64-command real-composition probe and source inspection.

| Claude native command | Aliases | heycode mapping / gap |
|---|---|---|
| `/add-dir` | — | Missing |
| `/advisor` | — | Same name, different semantics; heycode launches native advice |
| `/agents` | — | Present; heycode delegated-agent panel |
| `/artifacts` | — | Missing; artifact tool exists |
| `/auto-mode-setup` | — | Missing |
| `/autocompact` | — | Missing command; compaction configuration exists |
| `/autofix-pr` | — | Missing hosted feature |
| `/background` | `/bg` | Missing whole-session detach flow |
| `/branch` | — | Partial: /fork creates a session fork |
| `/btw` | — | Present |
| `/bug` | `/share` | Missing feedback flow |
| `/cd` | — | Missing session directory-switch command |
| `/chrome` | — | Missing command; browser integration exists |
| `/clear` | `/reset`, `/new` | Partial: /new |
| `/color` | — | Missing; /theme is broader |
| `/compact` | — | Present |
| `/config` | `/settings` | Present; settings behavior differs |
| `/context` | — | Present; explain measurement and growth |
| `/copy` | — | Present; verify selection variants |
| `/usage` | `/cost`, `/stats` | Present; aliases and provider limit views differ |
| `/design-login` | — | Missing hosted integration |
| `/desktop` | `/app` | Missing desktop handoff |
| `/diff` | — | Present |
| `/effort` | — | Present |
| `/exit` | `/quit` | /quit present |
| `/export` | — | Present |
| `/fast` | — | Missing provider-specific toggle |
| `/feedback` | — | Missing |
| `/focus` | — | Missing |
| `/fork` | — | Same name; background fork behavior differs |
| `/goal` | — | Present |
| `/heapdump` | — | Missing runtime-specific diagnostics |
| `/help` | — | Present |
| `/hooks` | — | Present |
| `/ide` | — | Missing |
| `/import` | — | Partial: /agent-config import, not whole configuration |
| `/init` | — | Present |
| `/insights` | — | Missing |
| `/install-github-app` | — | Missing hosted integration |
| `/install-slack-app` | — | Missing hosted integration |
| `/keybindings` | — | Partial: /keymap |
| `/list-agents` | `/peers` | Missing command; /agents and list_tasks are adjacent |
| `/login` | — | Partial: /connect |
| `/logout` | — | Present |
| `/mcp` | — | Present; verify management actions |
| `/memory` | — | Missing dedicated native panel |
| `/mobile` | `/ios`, `/android` | Missing mobile product integration |
| `/model` | — | Present |
| `/passes` | — | Not applicable to local runtime |
| `/permissions` | `/allowed-tools` | Present; alias absent |
| `/plan` | — | Present |
| `/plugin` | — | Partial: /plugins plus top-level management |
| `/powerup` | — | Missing optional onboarding |
| `/privacy-settings` | — | Missing account-specific UI |
| `/radio` | — | Not applicable to core runtime |
| `/rate-limit-options` | — | Missing provider-specific recovery UI |
| `/recap` | — | Present |
| `/release-notes` | — | Missing |
| `/reload-plugins` | — | Missing direct slash action |
| `/reload-skills` | — | Missing direct slash action |
| `/remote-control` | `/rc` | Missing remote product feature |
| `/remote-env` | — | Missing hosted environment feature |
| `/rename` | — | Present |
| `/resume` | `/continue` | Present; alias differs |
| `/rewind` | `/checkpoint`, `/undo` | Present; aliases differ |
| `/sandbox` | — | Present; verify supported transitions |
| `/schedule` | `/routines` | Partial: schedule tools, no equivalent slash flow |
| `/scroll-speed` | — | Missing dedicated control |
| `/security-review` | — | Present; review scope differs |
| `/setup-bedrock` | — | Partial: provider connection flow |
| `/setup-vertex` | — | Partial: provider connection flow |
| `/skill-doctor` | — | Present |
| `/skills` | — | Present |
| `/status` | — | Present |
| `/statusline` | — | Missing customization flow |
| `/stickers` | — | Not applicable to core runtime |
| `/stop` | — | Present; heycode targets operations, not whole detached session |
| `/subtask` | — | Missing convenience command; task tool exists |
| `/tasks` | `/bashes` | Present; primary UX needs domain separation |
| `/team-onboarding` | — | Missing optional report flow |
| `/teleport` | `/tp` | Missing cloud-session handoff |
| `/terminal-setup` | — | Missing terminal-specific setup |
| `/theme` | — | Present; palette and semantic application need work |
| `/tui` | — | Missing renderer-switch command |
| `/ultrareview` | — | Cloud-review compatibility entry; no equivalent hosted flow |
| `/upgrade` | — | Not applicable to local runtime |
| `/usage-credits` | `/extra-usage` | Missing account-specific feature |
| `/voice` | — | Present; local implementation |
| `/web-setup` | — | Missing hosted integration |
| `/workflows` | — | Present |

`/ultrareview` is retained as a compatibility row because the documentation still lists it separately; it now points into the bundled review flow. `/schedule` is listed as a native entry in the public reference, although the installed initialization inventory exposes a bundled skill with the same name. Treat this as a native entrypoint backed by a conversational workflow, not a simple local scheduler alias.

**Excluded bundled skills/workflows:** batch, claude-api, code-review and its review alias, dataviz, debug, deep-research, design, design-sync, doctor/checkup, fewer-permission-prompts, loop/proactive, run, run-skill-generator, simplify, verify, workflow-authoring. User-installed skill/plugin commands are also excluded.

**Removed commands:** pr-comments, ultraplan, vim. heycode may retain its own useful `/vim`; removal from Claude is not a reason to remove it here.

**Initialization-only entries:** design-consent, design-revoke, `__remote-workflow`, and workflow-launch-exec were seen in the installed initialization response but are not presented as public command requirements. Internal implementation hooks should not become heycode product commands just to inflate parity.

### All registered heycode slash commands

- `/help`, `/plugins`, `/compact`, `/title`, `/quit`, `/recap`, `/btw`, `/output-style`
- `/questions`, `/answer`, `/lmstudio`, `/status`, `/doctor`, `/permissions`, `/sandbox`, `/config`
- `/context`, `/usage`, `/web`, `/health`, `/init`, `/skills`, `/skill`, `/skill-doctor`
- `/agent-config`, `/plan`, `/tasks`, `/ps`, `/stop`, `/goal`, `/review-runtime`, `/scripts`
- `/attach`, `/document`, `/fallback`, `/provider`, `/model`, `/effort`, `/connect`, `/logout`
- `/profile`, `/diff`, `/copy`, `/mention`, `/review`, `/advisor`, `/security-review`, `/theme`
- `/keymap`, `/vim`, `/settings`, `/new`, `/resume`, `/fork`, `/rename`, `/archive`
- `/delete`, `/export`, `/rewind`, `/voice`, `/mcp`, `/agents`, `/hooks`, `/workflows`

## Updated requirement: stable definitions and prompt caching first

This section incorporates the user’s final steering and supersedes earlier suggestions to expand ToolSearch in Phase 2. **Do not build a new search-only tool architecture now.** Keep the current intended tool set available. Tool search for optional custom/MCP tools is a later project; its existing implementation is not to be deleted incidentally.

### What “send definitions once” must mean

Build one canonical tool-definition snapshot for a stable session configuration. Keep it at its protocol-defined position, with stable names, descriptions, schemas and ordering. Do not append a new prose copy of the definitions to every user/assistant/tool message. Do not insert synthetic reminder messages, timestamps, fluctuating status banners or reordered history to maintain tool availability.

There is an essential protocol distinction: **one logical definition in the model context is not necessarily one network transmission**. Stateless request APIs require the current request’s tool catalog and applicable history again. Reusing their prefix through provider caching avoids repeated full-price processing; omitting required schemas can make tools unavailable. Definitions should not be moved into the first user message as a substitute for native tool declarations.

Only use a send-once transport optimization when that particular provider/session protocol explicitly retains tool configuration and a tested continuation path proves it. A conversation ID alone is not proof that every configuration field persists. Reconnect, expired state, model changes and fallback must safely reconstruct the required request. No provider-independent “drop tools after request one” flag should be implemented.

### Lessons verified from Claude and OpenAI documentation

Claude Code documents a layered cached prefix and explains that ordinary appended conversation content preserves the earlier prefix. It reports cache reads/writes and identifies some miss causes. Its documented subagent behavior also distinguishes a child’s own cache from the parent’s. This supports stable initialization plus append-only progress, not fabricated repeated setup messages. [Claude Code caching](https://code.claude.com/docs/en/prompt-caching)

Anthropic’s API caches tool definitions using cache controls; deferred discovery can append references later. The immediate implementation should use the supported cache mechanism with stable definitions. Deferred discovery remains future scope. [Tool caching](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-use-with-prompt-caching)

OpenAI’s current API guidance explicitly recommends stable tool definitions, ordering and schemas, and supports model-dependent cache controls and append-only changes. Apply the contract for the selected model, not a universal retention constant copied from another model generation. The fetched documentation establishes OpenAI API behavior; it does **not** establish that every current Codex client sends definitions only once over the wire. No such Codex-specific implementation claim is made here. [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)

OpenAI’s engineering account of the Codex agent loop describes tool definitions as request configuration and tool outputs as additions to the next model input. It explicitly describes a prior MCP tool-ordering bug that caused cache misses, and preserving previous input as the prefix of the next request. Add deterministic ordering and prefix-regression checks so heycode does not repeat that failure. This is direct evidence for preserving request structure, not for deleting tool schemas after the first request. The article documents the harness at publication time; newer transport behavior still requires its current API contract. [Codex agent loop](https://openai.com/index/unrolling-the-codex-agent-loop/)

OpenRouter documents `usage.prompt_tokens_details.cached_tokens` and `cache_write_tokens`, with cache counters available when the underlying route supports them. Its caching guide also describes provider affinity. Capture these fields in the actual route before judging the result; support for the precise Muse model is not established by a generic gateway caching page. [OpenRouter usage](https://openrouter.ai/docs/cookbook/administration/usage-accounting), [OpenRouter caching](https://openrouter.ai/docs/guides/best-practices/prompt-caching)

### What this audit proves about heycode already

All nine captured request headers had the same canonical tool-definition hash and the same system-text hash. Therefore the repeated approximately 31 KB catalog is **not evidence that heycode appended nine catalog copies to the conversation**, nor evidence of a cache miss by itself. It is repeated request configuration. Full wire-prefix stability beyond these two fields remains to be measured.

The shared Chat stream parser records prompt/completion totals but does not preserve cache counters in the inspected `TokenUsage` path. This session’s durable usage likewise contains only those totals. Consequently we cannot truthfully calculate its cache hit ratio from the current records. Other provider modules already contain cache controls and cache usage types; the work is to connect and validate them end to end, including the actual OpenRouter route, rather than recreate all caching infrastructure.

### Implementation contract

1. **Canonical initialization:** deterministically serialize and hash system text and tool definitions. Freeze incidental metadata at initialization. Record a configuration revision separate from conversation messages. Use a stable cache-routing key where supported; do not generate a new key every turn.
2. **Append-only conversation:** persist authoritative message IDs and causal order. New user input, assistant output and tool results append once. Preserve tool-call/result pairing and provider continuation items. Deduplicate replay and completion notifications. UI progress events do not become model messages unless semantically required.
3. **Explicit changes:** when permissions, tools, model, guidance or directory truly change, record the reason and apply a supported transition. Never preserve a cache at the expense of a revoked permission. Do not silently alter an old message; use the provider-supported update mechanism or start a new measured prefix revision.
4. **Protocol-correct transport:** resend stable schemas where required; use retained session state only where supported and verified. Compaction and fallback are explicit boundaries. Retain original messages for audit/replay without accidentally sending both original and compacted context.
5. **Provider-aware cache setup:** validate Anthropic breakpoints, OpenAI model-specific options, supported gateway forwarding and route affinity. Do not assume Anthropic headers or OpenAI cache options apply to `meta/muse-spark-1.3` through OpenRouter. Verify that exact provider’s capability and usage fields before claiming savings.
6. **Usage plumbing:** persist uncached input, cache-read input, cache-write input, total input, output, provider, model, request ID and cache policy where supplied. Distinguish missing counters from zero. Anthropic totals and OpenAI totals require different normalization; cached input must not be added twice.
7. **Diagnostics:** expose stable-prefix hash, changed component, cold/warm status, supported/unknown cache telemetry, and cache reads/writes in `/usage` and `/context`. Keep raw secrets and opaque payloads out of diagnostics.
8. **Future tool search:** later evaluate native always-available tools plus deferred custom/MCP tools. Keep definitions stable or append them through a documented tool-loading mechanism. Do not repeatedly rewrite the prefix as tools are discovered.

### The 98–99% target, defined honestly

The user’s **98%+ target is an optimization acceptance target**, not an unconditional promise from the provider. Report three different metrics:

- **Input token cache ratio:** total cache-read tokens divided by total input tokens, with provider-specific normalization. This is directly useful for cost analysis.
- **Warm reusable-prefix efficiency:** cache-read tokens divided by the eligible unchanged prefix tokens for warm requests, where the provider exposes enough evidence to measure it. Target at least 98% on the controlled stable-prefix workload. If eligibility cannot be measured, report unknown rather than substitute a guess.
- **Request hit frequency:** requests with a nonzero cache read divided by measured requests. This can be high even when most tokens are uncached, so it cannot stand in for savings.

Cold starts, new content, different agent prefixes, expiry, model changes and compaction affect these differently. Even perfect prefix reuse cannot make 98% of every request cached: a 20,000-token reused prefix plus 1,000 new input tokens yields at most about **95.2%** input-token reuse. Caching also does not remove cached tokens from the model’s context window, and 98% cached input is not 98% off the total bill.

**Acceptance workload:** record cold and warm runs separately; use a fixed model/provider, a stable tool catalog, a short multi-turn conversation, sequential and parallel tools, five agents, a long result, restart, explicit tool configuration change, fallback and compaction. Preserve message identities and compare prefix hashes at every step. Report actual cache counters and cost when available, not inferred savings from serialized byte size. Show parent and child results separately plus an aggregate. Do not pad prompts or manufacture extra calls to improve the ratio.

Add this work to **P2.0**, before adding tools or changing task schemas. No cache-performance claim is complete until the actual configured provider produces supporting telemetry.

## Context preservation, quality and cost: what can actually be guaranteed

**Latest user constraint:** include an optimization as quality-preserving only if that property is established. Do not promise that summarization, retrieval, masking, fewer reasoning tokens, cheaper models, or extra agents preserve quality universally. Do not claim that heycode beats competing agents without a controlled comparison.

### The safe immediate choice

Use provider prompt caching while keeping the model input intact. This reuses processing of the same prefix rather than substituting a shorter description of it. OpenAI explicitly documents that prompt caching does not change output generation; identical requests can still produce different answers because generation is not deterministic. This establishes a mechanism that does not discard context, **not a guarantee of a correct answer, a cache hit, a particular discount, or identical output**. [OpenAI caching](https://developers.openai.com/api/docs/guides/prompt-caching)

The previous section's stable-schema and message-order contract is therefore the first cost optimization. Caching reduces repeated processing and eligible input cost; it does **not** reduce occupied context tokens. If the requirement is simultaneously fewer context tokens and guaranteed unchanged quality on every future coding task, the research reviewed here does not establish such a method.

### Keep original context recoverable

Separate durable storage from what is currently presented to the model. Preserve the original session events, tool outputs and relevant file versions with stable IDs and content hashes. A summary or selected excerpt must never overwrite the only copy. Replay must preserve message order, tool-call/result relationships and provider continuation items.

This is an engineering invariant that can be checked byte-for-byte. It guarantees recoverability only within the verified storage/retention contract; it does not guarantee that an agent will retrieve the right evidence later. A path alone is insufficient because a file can change. Retain the version or hash, then distinguish historical evidence from the current working tree. Do not inject the complete archive into every model request.

In heycode, build on the existing session log and compaction registry. `compact.rs` currently defaults to keeping two recent turns verbatim; that is a retention policy, not evidence that older details are no longer needed. Existing portable and native compaction remain continuity mechanisms, but must not be advertised as lossless or newly made more aggressive under the user's guarantee requirement.

### What is excluded from guaranteed-quality optimization

| Technique | Evidence / limitation | Phase 2 decision |
|---|---|---|
| Summarizing older context | Can omit subtle requirements and exact details | Do not add aggressive summary compression as a guaranteed optimization |
| Native opaque compaction | Provider-supported continuation with fewer tokens, not a universal quality guarantee | Preserve correct existing protocol behavior; evaluate before tuning |
| Masking older tool output | Published coding-agent experiments show useful results on tested configurations | Experimental only; not enabled by this report |
| Retrieval or a repository index | Can avoid reading irrelevant files, but can miss relevant evidence | No guaranteed-quality claim; validate retrieval recall first |
| Smaller model / lower effort | Reduces some unit costs but may cause more errors and retries | No automatic downgrade under the quality-preservation requirement |
| More subagents | Can help independent work but adds setup cost and coordination risk | Use for justified work, not as a generic savings strategy |
| Blindly deduplicating repeated text | Repetition may be intentional or evidence from a new execution | Prevent duplicate delivery by event ID; do not delete semantically distinct events |
| Smaller tool results | Can omit the line that explains a failure | Require explicit paging and full-result access; benchmark before tightening caps |

Anthropic describes compaction, structured notes and targeted retrieval, but explicitly warns that overly aggressive compaction can lose critical context. Its tool-design guidance recommends relevant outputs and evaluation, not a zero-loss guarantee. [Context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents), [Tool design](https://www.anthropic.com/engineering/writing-tools-for-agents)

OpenAI documents native compaction as carrying key state forward. For standalone compaction, the returned window is the canonical continuation and should not be further pruned. This is a protocol requirement, not proof that arbitrary future coding questions retain identical performance. [Compaction](https://developers.openai.com/api/docs/guides/compaction)

The *Complexity Trap* study compares observation masking with summarization in software-engineering agents and finds substantial efficiency gains in its evaluated settings. This is evidence worth testing later, not a transferable guarantee for heycode, its current model, or every repository. [Research paper](https://arxiv.org/abs/2508.21433)

### How to establish whether heycode is better and cheaper

Optimize **cost per correctly completed task**, not token count in isolation. A short run that produces a bad patch is not a saving. Include input, cache writes/reads, output/reasoning, summarization, subagents, retries and verification in total cost. Report human repair/review time separately rather than hiding it.

Use two comparison tracks:

1. **Harness effect:** same model, provider, effort, repository state, permissions and tool access; compare current heycode with one optimization at a time. This isolates whether the harness improved results.
2. **Product effect:** compare actual Claude Code/Codex/heycode configurations on the same held-out task set, with version, model, resource budget and tooling disclosed. This measures the product experience, not merely the underlying model.

Include bug fixes, multi-file changes, test repair, refactoring, dependency changes, long sessions and interrupted/resumed work. Add context-retention traps: an early user constraint, a rejected approach, an unresolved failure, an exact file version, a dependency between work items, and a correction made before compaction. Hidden checks should test the intended behavior and regressions; an agent's own tests and self-reported success are insufficient.

For any future lossy technique, compare repeated paired runs and report task success, regressions, instruction violations, unsupported completion claims, total cost, latency and context-retrieval failures. Predeclare the task set and acceptable quality threshold. If the uncertainty interval allows a material quality reduction, do not label the optimization quality-preserving. A finite benchmark can establish evidence of no detected regression within its scope, never a universal guarantee.

**Release rule:** Phase 2 may improve caching and exact event handling now. New lossy context reduction remains outside the guaranteed-quality scope. Claims such as “98% cache reuse,” “cheaper than Codex,” or “higher quality than Claude” require their own measured evidence and clearly stated scope. No such claim has been established in this audit.

## Delivery sequence and acceptance gates

### P2.0 — Restore trust in the current flow

Fix Glob ordering and audit Grep; add useful error diagnostics. Separate opaque reasoning activity from readable content, including replay. Add per-request context and cache measurements, stable-prefix checks, and a benchmark of the captured five-file flow. Show and verify the selected provider-native web-search route. These should land before a catalog expansion increases prompt overhead.

**Acceptance:** the two-file fixture and repository glob succeed with correct capped ordering; the five-file scenario needs no discovery workaround; encrypted-only streams never create empty expandable rows; measured context, projected context, and billing totals are distinguishable.

### P2.1 — Introduce structured work and migrate TodoWrite

Create one durable work-item service with revision-safe create/get/list/update, scope, dependencies, assignment and tombstones. Adapt existing team tasks to it. Migrate old todo state without breaking history. Remove TodoWrite from current schemas and prompts. Rename primary agent spawning and agent listing while retaining old-call replay compatibility.

**Acceptance:** two agents update different items concurrently without loss; conflicting updates fail clearly; dependency cycles fail; multiple items may be active; repeated create requests are idempotent; resume reconstructs the same list. `task_list` never includes a shell process or agent conversation.

### P2.2 — Finish agent navigation and visual hierarchy

Add a composer-level agent entry, an agent-only navigator, accurate active/waiting/unread counts, and separate Work/Jobs/Teams views. Use the existing transcript tree as the anchor. Support long lists and narrow terminals. Preserve focus and draft across child navigation. Apply semantic theme roles, improve headings and inline code, and remove ordinary-work error coloring.

**Acceptance:** 1, 5, and 20 agents at 80×24, 100×45, and a wide viewport; long names; mixed completion/error/waiting; keyboard and mouse; streamed growth; scrolling; parent/child return; Unicode; no-color/light/dark. Screenshots must be paired with actual interaction assertions and compared in the same state.

### P2.3 — Complete teams

Expose roster management, shared work, mail, assignment, dependency readiness, orderly shutdown, archival and recovery. Link each team member to its actual conversation. Make bootstrap defaults explicit. Ensure one team’s controls cannot mutate another team’s state accidentally.

**Acceptance:** lead plus several members complete dependent work; one member fails; another is replaced or reassigned; a restart interrupts and recovers safely; mail is not duplicated; active members prevent unsafe deletion; work-item revisions remain consistent.

### P2.4 — Close core tool gaps and prove availability

Implement model plan entry, MCP resource access/readiness, richer file reads, structured review findings, and safe current-session worktree entry/exit. Expand LSP/Grep options only with explicit contracts. Make tool availability inspectable; defer new tool-search/discovery behavior. Validate configured web search and notebook/LSP/browser prerequisites with real integrations.

**Acceptance:** every core row has a schema, permission behavior, provider-request proof, successful execution evidence, and an actionable unavailable state. No mock-only success is labeled live parity.

### P2.5 — Slash-command completion and optional integrations

Prioritize `/tools`, `/subtask`, `/list-agents`, `/autocompact`, `/memory`, `/add-dir`, `/cd`, reload actions, context/usage aliases, and terminal controls. Define alias behavior centrally. Hosted routines, mobile handoff, remote control, hosted sharing, account billing and promotional commands are separate integration projects, not small missing local tools.

**Acceptance:** help and completion reflect registered/available/disabled states; aliases do not schedule unintended model turns; mid-turn commands respect timing rules; shortcuts remain conflict-free. Explicitly document supported, deferred and not-applicable rows.

## Regression matrix

| Scenario | Required evidence |
|---|---|
| Glob component/string ordering | Minimal fixture, real repo, cap boundary, deterministic output |
| Grep shared traversal | Multiple files with directory/file prefix collisions |
| Filesystem limits | Cancellation, symlink boundaries, denied root, unreadable descendant, special filenames |
| Reasoning | Empty/opaque/readable streams plus replay and actual clicks |
| Context | Request IDs, estimates, provider usage, schema footprint, growth attribution |
| Five agents | Live dispatch, stable IDs, individual results, no wrong counts |
| Agent navigation | Parent draft/scroll restored, child-specific usage, unread status |
| Concurrent work updates | Revision conflict, idempotency, dependencies, scoped permissions |
| Teams | Failure, reassignment, shutdown, restart, mailbox deduplication |
| Availability | Default, configured, unavailable, deferred, restored connections |
| Colors/layout | Same-state screenshots, narrow/wide viewport, light/dark/ANSI/no-color |
| Commands | Native-only inventory, aliases, dynamic gating, active-turn timing |

## Completion standard

The final Phase 2 handoff must state separately: implementation completed; controlled tests passed; live flows exercised; provider/integration coverage; remaining limitations. Re-run the exact user journey in a fresh real heycode session and inspect the result with both mouse and keyboard. The current audit’s failures remain open until that evidence exists. The earlier Phase 1 test count must not be reused as proof that these product gaps are closed.

## Mandatory Claude parity for every tool and command UI

This is an explicit user acceptance requirement, not optional visual polish. Only the heycode top banner may differ. Every tool presentation in the transcript and every command surface must match the corresponding live Claude UI, including Bash/shell execution, file reads, writes and edits, search and discovery, agents, teams, background work, provider tools and MCP tools. Tool names, arguments, output and status must remain truthful to the native operation; do not invent Claude-only capabilities or evidence.

For each applicable tool and command, compare the actual live reference and heycode at the same terminal size and equivalent execution state. Verify the title and summary, transcript indentation, spacing, symbols, colors, argument preview, running/working indicator, pending/queued presentation, grouped calls, success, empty results, errors, denied permissions, cancellation, bounded long output, truncation notice and expanded/collapsed details. Command pickers, confirmations, help, shortcuts, mouse hit regions and keyboard navigation are part of the requirement. Verify selection, expansion, scrolling, dismissal and return to the correct conversation without changing drafts or routing input to another owner.

Cover single calls and concurrent calls, multiple background shells with isolated output, and parent/child conversations. Include narrow and wide viewports and the supported dark, light and no-color modes. Where a state cannot be reproduced in live Claude, record the missing reference and leave that acceptance check open; renderer snapshots and unit tests alone do not establish visual parity.

Execution order: complete the bottom bar/background/agent flow alignment first, then align the tool and command inventory and native behavior, then close every tool/command UI comparison gap before declaring those items complete. This visual and interaction gate also applies during inventory implementation, so functional support alone is insufficient for completion. Record reference captures, native captures, interaction results and any remaining mismatch for every covered family in the tracker.

The internal-tool scope explicitly includes `read` → Claude Read, `write` → Write, `edit` → Edit, `bash` and background shell execution → Bash, `glob` → Glob, and `grep` → Grep. Audit every other internal tool for a Claude equivalent and use that equivalent’s UI as the reference. Matching labels alone is insufficient: summaries, arguments, result rendering, expansion, working state and error behavior must match.

The current per-tool verification matrix is [tool-ui-matrix.json](audits/tool-ui-matrix.json). Its 51 rows come from an actual native-runtime provider request in the local test journey. Advertisement alone is not proof of configuration, permission, remote execution or visual parity; those checks remain separate and explicit.

## Required file-tool efficiency and mutation safety

Latest user direction (supersedes the earlier 50-line request): read 200 lines by default, expose file size and line count plus explicit continuation, support efficient batch reads and multiple edits, and make mistakes detectable before committing changes. Implement bounded output and memory, one-based line offsets, explicit limits and version evidence. Add `read_many` with per-file results under an aggregate budget, and `multi_edit` that validates an ordered edit set before one atomic per-file commit. Use exact matches, stale-read rejection and dry-run previews; never silently guess an ambiguous edit. Make new-file creation the safe Write default and require explicit intent and a current revision for whole-file replacement. No tool can guarantee a model never makes a mistake; validation failures must be actionable and must preserve the file. These requirements are tracked as P2-FS03 through P2-FS06 and inherit the mandatory Claude-equivalent UI checks in P2-UI03.

### File-tool implementation contract

`read({"path":"src/main.rs"})` returns up to **200 lines** and **16 KiB** of raw text by default. Its result includes numbered content, `total_bytes`, `total_lines`, `lines_returned`, `lines_remaining`, and an opaque `revision`. Pass the returned `continuation` object directly to Read for the next page: it preserves the line/byte budget and checks the revision. Line offsets are one-based; explicit `limit` is at most 2,000 and `max_bytes` at most 256 KiB, subject to configured ceilings. Extremely long lines continue by `byte_offset` at a valid UTF-8 boundary. `partial_last_line` identifies an unfinished line. `page_line_ending` describes the returned page for exact edits. Exact line counting scans at most 64 MiB with bounded memory; larger files report unknown line totals (`null`) and `scan_limited`, while retaining exact file byte size. Requests beyond that scan fail with guidance to narrow with Grep.

`read_many({"files":[{"path":"a.rs"},{"path":"b.rs","offset":201}]})` supports up to 16 files. Each defaults to 200 lines; the batch shares a default 1,000-line/32-KiB budget (maximum 1,000 lines/64 KiB). Each entry reports read, error, or deferred independently. A deferred file was not read. Pagination metadata remains per file; aggregate budgeting counts original bytes, including CRLF bytes.

`multi_edit({"path":"a.rs","expected_revision":"<read revision>","dry_run":true,"edits":[{"old_string":"old","new_string":"new"}]})` validates up to 32 ordered exact replacements against one observed file. Later replacements see earlier proposed replacements. A missing or ambiguous match identifies the failed edit and commits none of the set. Remove `dry_run` after reviewing the preview to commit, retaining the revision check. `edit` supports the same revision and preview fields for one replacement. No-op edits preserve the file. Diff previews are bounded and identify truncation; replacement counts cover the operation. Mutation input is capped at 256 KiB; edited files and prospective expanded content are capped at 64 MiB. Replacements never guess whitespace or ambiguous locations.

`write({"path":"new.rs","content":"..."})` creates a new file and refuses to overwrite an existing path. Whole-file replacement requires `mode:"replace"` and `expected_revision` from a current Read. Identical content returns an unchanged receipt. Revision tokens describe the observed filesystem metadata; they are not content hashes. Local writes check the captured version immediately before atomic publication. Filesystem providers must implement checked mutation operations explicitly; unsupported providers return an error rather than falling back to partial edits.

Controlled tests cover paging, UTF-8 boundaries, stale revisions, batch budget exhaustion, atomic failure, previews, Unicode edits, no-op writes, and an intervening write whose later read refreshes the observation log. Provider-wire and live terminal validation remain separate requirements; implementation alone does not close the UI parity gate.

Bounded file receipts are serialized as complete compact JSON in both native and delegated tool-result paths. The older generic 32,768-character clipping would corrupt large batch receipts and has been bypassed for these producer-bounded file objects. A separate 2 MiB serialized ceiling accommodates worst-case JSON escaping at explicit maximum read limits; it is not the default read budget. Over-limit receipts produce an explicit error advising inspection before retrying. The model retains page continuation and revision metadata.


### Search bounds and coverage contract

Local Glob/Grep traverse at most 100,000 entries and retain at most 16 MiB of discovered path bytes before sorting the observed subset by normalized output path. An interrupted traversal is explicitly partial; its observed match total is a lower bound. Descendant read failures are counted while readable siblings remain searchable; root failures remain errors. The portable result contract cannot represent non-UTF-8 paths, control characters, backslashes, or colon-bearing path components; those paths are skipped with a coverage notice, never lossy aliases.

Local Grep scans in 8 KiB chunks on a blocking worker with cancellation, capped at 64 MiB per file and 256 MiB across a search. Lines above 1 MiB, non-UTF-8 lines, and non-text control lines are skipped and counted. Binary-probe exclusions retain the existing text-search semantics. Matching excerpts retain up to 4 KiB each; aggregate retained search rows are bounded to 24 KiB, with a separate coverage/output notice. Match counts remain exact for fully scanned eligible text even when excerpts/rows are omitted; unreadable/skipped content and scan/traversal ceilings mark the count as a lower bound. Use a narrower path/include or Read on a reported file to investigate omitted material. These limits do not add richer glob syntax or regex options.


### Update preview implementation

Edit/MultiEdit now attach source-backed surrounding context to the existing bounded diff. Context uses the same in-memory snapshot as the mutation; up to three unchanged lines on each side are retained. Structured display rows share a 16 KiB text budget across a batch, with shorter context excerpts and explicit truncation. Added/removed line counts describe each edit's affected first replacement span; repeated replacements are labelled as first-replacement previews. Multiple sequential edits are labelled individually and do not claim to be a net final patch summary. Dry runs say “Would add/remove”; unchanged output does not claim a mutation.

Compact and expanded Update cards render numbered context with removed/added backgrounds (indexed 52/22 and markers 167/77, from the captured Claude terminal reference; basic and no-color fallbacks remain supported). This closes a specific visual mismatch, not the mandatory all-tool/all-command parity gate. Broader viewport/state comparisons remain in the tracker.
