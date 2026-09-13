# heycode / Claude Code: subagent and background-task comparison

Date: 10 September 2026. Status: implementation complete and rebuilt for user retest.

## Implemented and verified

All T1–T14 items in the [implementation checklist](tui-parity-implementation.md) are complete. Background shell/terminal jobs have no automatic deadline; their inspector retains command, directory, timing, and exit reason. Main and child conversations use the same semantic message/tool rendering, with independent navigation focus, drafts, and scroll position. Multiple messages queue below the activity indicator, Up recalls unclaimed messages atomically, and Esc/Ctrl+C interrupt the correct owner. Native descendant concurrency defaults to unlimited, and adjacent spawns render as one clickable tree.

Verification: 1,415 passing tests across the six verified packages, clean targeted Clippy, documentation checks, and a rebuilt CLI. The live read-only replay retained the configured `meta/muse-spark-1.3` model, launched three children, and completed all 12 ticker outputs in 120.1 seconds. The voice-model Git status was unchanged. The final native-runtime PTY replay used a deterministic local HTTP provider and passed all 12 captured stages; this verifies UI behavior independently of provider wording and latency.

| Evidence | Result |
| --- | --- |
| Grouped spawns (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/spawn-trees-20260910T091831Z-377c5d.png`) | Three simultaneous children in one tree |
| Queued messages (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/beta-two-queued-20260910T091831Z-9bc3f1.png`) and recall (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/beta-recalled-20260910T091831Z-74f788.png`) | Two pending messages; both recalled into the owning composer |
| Cancellation (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/beta-cancelled-draft-preserved-20260910T091831Z-af64f5.png`) | Child stops while its recalled draft is preserved |
| Narrow child view (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/alpha-narrow-20260910T091831Z-3639f3.png`) | Navigation and composer retained at 45 columns |
| Final terminal inspector (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/terminal-input-completed-20260910T091831Z-9f65f4.png`) | Output, input, retained metadata, clean completion, no idle spinner |
| Live two-minute ticker (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/live-ticker-completed-20260910T091230Z-2c84d7.png`) | Twelve ticks; no deadline; measured start/end and success |
| PTY replay result (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/pty-result-20260910T091831Z-7b35a4.json`) | All 12 stages passed on the rebuilt native CLI |

The live ticker image was captured before the final cosmetic cleanup; the final PTY images include that cleanup. The original audit below is retained as the pre-implementation baseline. Its gap descriptions and “not tested” statements describe that earlier run, not the final implementation verification above.

## Original audit verdict

The native heycode foundation works: it launches real concurrent subagents, preserves their configured model, exposes their output, accepts direct follow-ups into continuable children, launches background terminals, and reports completion/failure. The biggest experience gap is how these capabilities are presented. Claude treats a child as another conversation in the same interface. heycode treats it as a technical task-output viewer, with IDs, lifecycle events, raw tool output, and a separate navigation model.

There is also a confirmed runtime problem: the requested two-minute background terminal was killed after about 30 seconds in heycode. Claude completed all 12 ticks. This needs fixing before visual parity can be considered complete.

This report compares the installed/build artifacts actually exercised. It is not a certification of all subagent features or of the voice-model product.

## Required layout constraint

The user explicitly does not want a right-side diff view, split-pane inspector, or large rectangular side box. Keep the product in a single-column terminal conversation. Use a compact agent selector and lightweight inline or bottom details; any future change/diff preview must stay inline in that same flow. The captured reference screenshots are evidence, not a requirement to copy large bordered containers. The dotted region to the right of some screenshots is fixed-size tmux capture padding and is not a proposed panel.

## Setup and boundaries

- Both applications ran in `/Users/naresh/Work/Personal/voice-model` in independent tmux sessions, socket `heycode-tui-lab`.
- Claude Code 2.1.267 used `claude-opus-5`, high effort, the existing Claude Max account, and manual permissions. Child transcript records confirmed Opus 5.
- heycode used the existing `target/debug/heycode` binary, version 0.1.0, built at 17:40, with the existing `meta/muse-spark-1.3` / OpenRouter configuration. Child request records confirmed that model. No rebuild or model override was performed.
- The tests read README.md and PROJECT_CONTEXT.md, slept, and printed terminal markers. No product development, builds, services, or file edits were requested from either agent.
- The voice-model Git status before/after was identical. Tool records support the read-only scope. This was not a full filesystem hash audit.
- Inputs were sent through tmux. Screens were actually rendered through local, read-only ttyd views. Initial captures used Playwright/Chrome; the navigation audit used the Codex in-app browser. Mouse checks injected standard terminal SGR press/release events at observed terminal cells; native OS pointer/clipboard behaviour was not tested.
- Initial evidence used 180×58 terminal cells after the screenshot browser attached. Navigation comparisons used 100×45 cells in both applications. Browser canvas sizes differ in some captures; the dotted area outside the fixed terminal is tmux padding, not either app's design.

## 1. Execution results

| Test | Claude Code | heycode | Conclusion |
|---|---|---|---|
| Two native children, same configured model | Confirmed in child records | Confirmed in child records | Pass for this scenario |
| README worker sleep | 07:49:16–07:49:26 UTC | 07:49:30–07:49:40 UTC | Both actually slept 10 seconds |
| Context worker sleep | 07:49:19–07:49:29 UTC | 07:49:37–07:49:47 UTC | Both actually slept 10 seconds |
| Concurrent sleep windows | 7 seconds overlap | 3 seconds overlap | Real concurrency in both; not a latency benchmark |
| Assigned document reads | Both succeeded | Context succeeded; initial README denied; recovery succeeded | Denial was controller interference, not a delegation failure |
| Continuable background children | nav-readme and nav-context stayed available | `task(mode=continuable, background=true)` produced idle children | Supported by both |
| Follow-up entered in selected README child's UI | Returned `NAV_CHILD_OK` and README path | Returned `NAV_CHILD_OK README.md` in the child transcript | Direct child routing and retained context worked |
| Two-minute background terminal | Exit 0, 12/12 ticks | Failed after ~30 seconds, 3/12 ticks | Confirmed heycode functional gap |
| Additional 20-second heycode terminal | Not rerun; longer reference already passed | Live output visible; `BG_READY`, then `BG_DONE`, completed | Basic background terminal works below the deadline |

The initial README denial happened when the controller's Down/Down/Enter navigation sequence reached a newly displayed approval modal and selected Reject. The worker accurately reported the limitation. A fresh native recovery worker read README.md successfully. Do not attribute this denial to a model/provider failure or claim the original two-read run was clean.

Evidence: execution records (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/execution-evidence.json`), heycode background records (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/heycode-background-evidence.json`), Claude background records (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/claude-background-evidence.json`).

## 2. What the navigation actually does

1. **Claude background entry:** with a shell running, Down from the empty composer highlighted `1 shell`. Enter opened a bottom `Background` dialog. It grouped agents/team members separately from shells.
2. **Claude shell details:** selecting the shell opened a bottom detail panel with running status, runtime, full command, and an output box that advanced through `UI_TICK` lines. The surrounding parent transcript remained visible. Esc returned out of the detail flow.
3. **Claude child foreground:** `/tasks` provided access to retained teammates. Selecting nav-readme opened an agent detail card; `f` foregrounded its conversation. It used the normal assistant transcript UI and a composer explicitly labelled `Message @nav-readme…`.
4. **Claude radio/focus behaviour:** the filled radio showed the active conversation; a separate arrow showed keyboard focus. Down entered the visible roster, further Down moved selection, Up reversed direction, and Enter changed the viewed conversation. Clicking nav-readme via terminal mouse events changed the active radio and transcript. Returning to main restored the parent conversation.
5. **Claude roster caveat:** the compact roster changed over time and did not consistently retain every idle teammate. `/tasks` still exposed all retained teammates. The audit confirms the navigation mechanism, not that all idle children always stay in the compact list.
6. **heycode task entry:** Down from the ordinary empty composer did not enter a conversation roster. Ctrl+T opened a separate task list. Up/Down moved within that list; Enter or a mouse click opened the selected item's output. Esc returned to the list; another Esc closed it.
7. **heycode child follow-up:** opening an idle continuable child exposed `Message nav-readme worker · Enter sends`. A direct follow-up reached that child and received the expected marker. However, its transcript was rendered as plain event/output lines rather than the main conversation components.
8. **heycode terminal interaction:** clicking a running terminal opened the full task-output area with `Terminal bg-live20 · Enter sends a line`. Completion changed it to read-only. Output was retained. Alt+M exposed metadata. Interrupt and stream-switch controls were visible but were not exercised.

Keyboard batches must allow the UI to render between focus movement and activation. This audit used separate key steps for the final navigation checks; it does not treat earlier rapid-key/controller races as proof of an application key-handling defect.

## 3. Why Claude looks and feels more polished

Claude has a clearer visual hierarchy: conversation first, compact activity second, diagnostics on demand. In the selected child it collapsed successful work into `Read 1 file, ran 1 shell command`, showed the response as an ordinary assistant message, and kept the target identity beside the composer and in the radio roster.

heycode's selected child displayed the complete numbered file read, raw tool-call identifiers, `awaiting ordered commit`, `Turn settled: Stop`, queue IDs, owner/session IDs, and token metadata together. These are useful diagnostic facts, but they obscure the answer and make a small task occupy a long transcript. The same readable child answer became harder to find without any underlying model-quality difference.

Claude's background panel is a compact inspector attached to the conversation. heycode replaces the conversation with a generic output page. Its footer spends space on token/model fields that do not apply to a shell job, while omitting the command, useful runtime information, and the failure explanation.

The improvement should therefore be about hierarchy, grouping, consistent navigation, and accurate state before decorative changes to colours or the mascot.

### Selected child: reference

Claude child conversation with normal transcript and selected-agent radio (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/claude-child-transcript.png`)

### Selected child: heycode

heycode child transcript exposes raw file output and internal lifecycle messages (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/heycode-child-followup.png`)

### Background shell: reference

Claude bottom shell panel with command runtime and output (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/claude-shell-details.png`)

### Background shell: heycode

heycode live terminal uses the whole task-output area and lacks runtime metadata (local-only evidence: `docs/audits/assets/subagent-comparison-2026-09-10/heycode-running-shell.png`)

## 4. Confirmed gaps and priority

### G1 — High: background terminal inherits an unsuitable deadline

**Observed:** the 120-second ticker stopped with `terminal timed out` after three ticks. A subsequent 20-second terminal completed normally.

**Source trace:** `BackgroundTerminalTool` in `crates/heycode-agent/src/execution_jobs.rs:844` exposes only command and label and creates a `ShellRequest` without a deadline override. `crates/heycode-cli/src/lib.rs:1887` configures the shared shell from `tools.bash_timeout_ms`; `crates/heycode-exec/src/shell.rs:430` applies that default when the request omits a timeout. The configuration default is 30,000 ms in `crates/heycode-config/src/lib.rs:157`.

**Fix:** give background execution an explicit, documented lifetime policy and per-job timeout field, with an appropriate bounded background default. Show the deadline in the approval/detail UI. Keep foreground command budgets independent. Preserve cancellation/process ownership; do not simply disable every timeout globally.

**Acceptance:** the exact two-minute ticker prints all 12 ticks and exits 0; a deliberately shorter explicit deadline reliably times out and displays its reason.

### G2 — Medium: no shared main/child conversation navigation

**Observed:** Claude's visible roster supports focus, radio selection, clicking, and direct conversation switching. heycode requires Ctrl+T, an ID-heavy list, and a separate output surface. A direct child message already works, so this is mainly a navigation/presentation gap.

**Fix:** add one compact navigation region for background work, main, and child conversations. Keep keyboard focus separate from the active conversation. Use the same stable task/session identity for keyboard and mouse actions. Preserve each conversation's draft and scroll position. Enter switches the viewed conversation; entering text routes only to the selected owner.

**Acceptance:** with two children and a shell, Down enters the background entry, then main/child rows in a documented order; Up reverses it; Enter/click selects; the filled radio, transcript, composer label, and message destination always agree. Multiline editing and prompt history remain usable.

### G3 — Medium: child transcripts are raw diagnostic output

**Observed:** the child view dumps file contents and internal execution messages. It lacks the ordinary main transcript's grouping and message presentation. Long paths and words split awkwardly at the terminal width.

**Source trace:** `task_render.rs:281` obtains `output_lines`, wraps them, then renders them with `Line::raw`; `task_console.rs` builds textual event lines. This is a separate path from the primary transcript renderer.

**Fix:** project child events into the same semantic message/tool blocks used by main. Default to compact completed-tool summaries; expand individual tools on demand. Keep exact raw events in a debug/details view. Group adjacent compatible successful tool calls within a turn; never bury an approval, failure, or changed outcome inside a success summary.

**Acceptance:** a child that sleeps and reads a file presents a short tool group and its answer. Expansion reveals the original output, ordering, timestamps, and error details without losing information.

### G4 — Medium: task counters count different things from the visible list

**Observed:** two foreground child delegations produced `Tasks 4 · 4 running`: two parent task-tool rows plus two child rows. After completion, the footer said `Tasks 2` while the open list still displayed four completed rows. Later lists also mixed wrapper tool calls, shell jobs, follow-up jobs, and children.

**Source trace:** `RegistryTaskSource::snapshot` starts with root tool records then appends child records. `app/workflow_navigation.rs:8` filters completed Tool/Job records from the strip, but the expanded list uses the underlying records. This explains the count/list mismatch; it is not evidence of four child processes being created.

**Fix:** define the user-visible units as conversations and background jobs. Correlate wrappers with their child/job, and keep wrapper calls inside transcript/details. Have the badge and list share one filtered data set or explicitly label separate active/history totals.

**Acceptance:** two children show two children throughout their lifecycle, with history clearly separated and no ambiguous total.

### G5 — Medium: child approvals are hard to identify and waiting state is inaccurate

**Observed:** a child `bash` approval was on screen, sometimes with another request queued, while the task strip showed all four entries running and zero waiting. Child dialogs showed the operation/arguments but not the originating worker. The two identical sleep requests were visually indistinguishable.

**Fix:** carry task/session identity with each approval and render the worker name prominently. Project pending/resolved approval state into that child's status. Restore prior focus when closing the modal and reject stale queued navigation activations. Inspect the event forwarding into `ObservedTask.pending_approvals`; the current source already has a waiting projection, but this build's observed state did not reflect it.

**Acceptance:** the correct worker becomes `Waiting for approval`, queue counts agree, and accept/reject resolves only that request. Test approval arrival during navigation deterministically. The controller-caused README rejection is not itself classified as a permission-system defect.

### G6 — Medium: background details omit the information needed to diagnose a job

**Observed:** the failed ticker's metadata showed `Execution elapsed: unavailable`, `Started: unavailable · settled: not settled`, `Directory: unavailable`, and no command or timeout reason, despite status `failed`. The running short job also lacked elapsed time. Token/model placeholders consumed space although these are not shell metrics.

**Fix:** add a lightweight bottom shell inspector in the same column: label, status, command, cwd, runtime/deadline, exit code or failure reason, and live output. Use restrained separators rather than a large boxed side panel. Hide inapplicable token/model fields. Offer full-screen output as an optional expansion. Bind telemetry to the execution/job owner rather than estimating it from assistant prose.

**Acceptance:** both running and settled jobs retain their actual timings and result; timeout is visible directly in the inspector without asking the model to explain it.

### G7 — Medium: inline-code rendering corrupts multiword spans

**Observed:** a correct stored model message containing the single code span `readme recovery reader` appeared as “`readme` recovery reader ``”. A longer shell-command span was also broken across literal delimiters. This was verified against the stored assistant message, so it is not different model wording.

**Source trace:** `markdown.rs:221` splits the text into whitespace-delimited words before finding pairs of backticks, preventing a multiword code span from being treated as one semantic span.

**Fix:** retain the Markdown parser's styled inline spans through word wrapping; do not reconstruct and reparse code spans word by word. Preserve nested-list indentation and intentional block spacing as part of the same renderer pass.

**Acceptance:** multiword inline code, commands containing spaces, adjacent punctuation, long paths, and narrow terminal widths render without stray delimiters or lost content.

### G8 — Low/medium: stale notices and technical labels distract from the selected item

**Observed:** after moving from a child follow-up to the ticker, its output footer still said `Child follow-up admitted as job-3`. The selected shell had inherited a notice about a different child. Long internal IDs and default channel labels also dominate the ordinary list/detail views.

**Source trace:** `task_render.rs:188` prioritizes the console-wide notice before the selected row's details.

**Fix:** key notices by owner/item and clear or replace them on selection changes. Prefer friendly labels, concise action/status text, and applicable controls. Put internal IDs and low-level lifecycle information behind Details.

**Acceptance:** selecting a different task never shows another task's delivery notice; ordinary views remain understandable without knowing job IDs or event terminology.

### Outcome distinction: completed turn versus completed objective

The README worker whose read was denied appeared as `completed`, with `0 failed`, although its task objective was incomplete. The worker and parent prose accurately disclosed the limitation. This is not necessarily a runtime-status bug: the model turn completed normally. The UI should distinguish lifecycle settlement from a blocked/partial objective, rather than marking every recovered tool error as a fatal agent failure.

## 5. Implementation plan

### Milestone 1 — Execution and state accuracy

Fix G1, G4, G5, and the missing job telemetry in G6. Extend the existing native execution and observation services; keep provider/model selection, ownership, approvals, and cancellation intact. Add narrowly targeted tests for deadlines, wrapper correlation, child approval states, and job settlement metadata.

This milestone comes first because an attractive UI must not silently kill background work or report misleading state.

### Milestone 2 — One conversation experience

Implement the compact main/child selector and shared child transcript rendering (G2/G3). Reuse `TaskSource`, native child observations, and the already working child follow-up route. Introduce explicit state for focused navigation item, selected conversation, open inspector, and per-conversation view state. Keep main and child histories separate while sharing visual components.

Primary areas: `app/task_navigation.rs`, `task_console.rs`, `task_source.rs`, `task_observation.rs`, `task_render.rs`, and the main transcript/render components.

### Milestone 3 — Background inspector and visual grouping

Create a lightweight bottom background list/detail flow within the same terminal column, with no right-side diff pane or large rectangular container. Then implement concise group labels, status hierarchy, appropriate metadata, selected-agent feedback, and item-scoped notices. Repair multiword inline code (G7) and apply one spacing/typography system to both main and child conversations. Reuse the existing palette; avoid a broad theme rewrite before the interaction works.

Primary areas: `task_render.rs`, `render.rs`, `markdown.rs`, `app/workflow_navigation.rs`, plus job telemetry projection.

### Milestone 4 — Replay the full interaction sequence

Use deterministic fixtures for exact UI assertions, then repeat these live scenarios with Claude Opus 5 and the existing heycode model:

1. Start two children; show them running and waiting for approval.
2. Navigate with Down/Up/Enter and mouse; verify focus versus selected radio.
3. Open each child; send distinct markers; verify only the selected child's history receives them.
4. Switch back to main, preserving unsent drafts and scroll position.
5. Start the two-minute background ticker; inspect live output in a bottom panel; let it complete.
6. Exercise a deliberate timeout and an explicit cancellation of a disposable job.
7. Inject an approval while navigating; prove it cannot inherit an unintended activation.
8. Repeat at narrow and wide terminal sizes and inspect actual screenshots.

Do not use generated wording, model speed, or identical pixel output as the cross-model pass condition. Compare state correctness, action routing, meaningful grouping, and layout stability.

## 6. What remains unverified

Native macOS pointer/clipboard behaviour, screen-reader accessibility, cancellation controls, close/reopen semantics, app restart/resume, multiple simultaneous failing shells, long-session output retention, per-conversation draft/scroll preservation, and full stress tests were not exercised. Visible controls are not counted as tested features. Current source was inspected to ground the plan; no claim is made that all unbuilt working-tree changes match the tested binary.

No implementation fixes, configuration changes, commits, or pushes were made. The delivered changes are this report, captured evidence, and the local testing harness. Both tmux sessions remain available in voice-model. Live read-only views are at http://127.0.0.1:7681 (Claude) and http://127.0.0.1:7682 (heycode).
