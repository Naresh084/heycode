# heycode-tui

`heycode-tui` is the plugin-owned terminal shell. It renders the durable/live
transcript and owns trust, onboarding, secret, approval, command confirmation,
profile, route, model, permission, MCP and plugin surfaces in explicit priority
order.

Model and effort pickers retain a typed backend owner and routing revision from
open through async completion and selection. Native discovery uses
`CatalogRegistry`; delegated discovery calls that runtime's
`AgentRuntime::models` and is never redirected to the native fallback. Replaced
or cancelled waits cannot populate a picker owned by another backend. The
effort picker renders exact choices, current/default badges and the same
keyboard semantics in full-screen and screen-reader modes; selection returns
the captured token for routing's final stale-configuration check.

The TUI contributes queued `/profile [name]`. It reads the effect-owned profile
service, highlights the current strict profile and returns a typed
`RecomposeProfile` outcome after validation. It never edits configuration or
rebuilds the world itself. Every exit/recomposition path cancels and joins turn,
command, authorization, doctor and model-refresh operations before returning to
the CLI composition owner.

The TUI also contributes immediate `/settings`. The command and running shell
share the `TuiHandle` panel inbox; the shell attaches the live `settings` and
`settings-ui` services before making it available. The browser renders every
schema-derived field or an explicit custom-panel delegation, retains only a
configured bit for secrets, and sends editable toggle/choice/text/number values
through expected-revision user-layer CAS. Managed, project, secret and
unrenderable values stay visible with refusal reasons. Successful commits state
whether the change applies live or after restart; stale commits reload the
authoritative rows.

CMD04 uses the same live panel inbox for `/mcp`, `/agents` and `/hooks`.
Capability owners that already own their command (`/plugins` and `/skills`)
emit an opaque panel request on the human-only UI bus. Production startup now
attaches the Settings-backed MCP management service, managed plugin lifecycle
and verified package index plus the composed skills, subagent and hook
registries before command dispatch. Missing services leave commands visible
with an unavailable/error reason; no command opens an empty fake surface.

Skills, agents and hooks share a bounded read-only catalog view. It exposes
skill invocation policy, provider tri-state delegation evidence, aggregate
live-child count and hook owner/event/phase/handler class without rendering
skill bodies, child ids across authorities, hook commands, prompts or
arguments. Full-screen and flat screen-reader projections share the same view.

U22 adds three exact UI-registry side-panel contributions: Diff, Jobs and
Agents. The persisted `cycle-side-panel` action ships on Ctrl+B and cycles those
views before closing. Diff projects only committed edit/write result diffs;
Jobs reads the effect-owned JobRegistry; Agents reads provider/preset
descriptors and aggregate state without exposing cross-owner child ids. All
three snapshots are bounded and control-free, and both full-screen and flat
renderers consume the same projection. Higher-priority trust, secret,
onboarding, approval and picker surfaces continue to preempt them.

The model picker optionally consumes `catalog-overrides`. Provider descriptors
remain unchanged evidence used by filters; user and trusted-project assertions
render beside them with source/direction, contradictions warn, and assertions
for absent canonical models stay visibly unmatched. A user `Supported`
assertion cannot satisfy provider-native Tools/Reasoning filters. Durable
enforcement remains CAT07 work.

Rich tool results render from durable typed metadata rather than raw media.
MCP-originated output receives an `UNTRUSTED MCP SERVER CONTENT` warning distinct
from Web provenance; live and replay views consume the same session facts.

ATT04 adds no public composer command or model-picker badge. When a hidden
experimental route produces durable `assistant/audio`, full and flat renderers
show only filename/MIME/duration/rate/channel/depth facts. Content addresses
and encoded samples never enter terminal cells. Live state, session replay and
app-server events reduce to the same `AudioOutput` item.

MCP11/MCP13 product attachment is exposed through `McpProductSession` and
`tui_plugin_with_mcp_bridge`. One opaque route is derived from the exact
pre-minted `SessionId`, and one router may be minted per server. The active loop
owns the broker activation: form elicitation accepts one schema-validated JSON
object, URL elicitation displays only credential-free HTTPS, Esc declines, and
loop teardown cancels every waiter so the transport sends no late reply.
Progress and structured logs are exact-route, bounded, terminal-control-safe
human `Info` rows in both full and screen-reader projections; none enters the
session/model log.

The same product object adapts MCP Prompt policy through the ordinary Agent
approval policy without server annotations and exposes the shared O09 hook
adapter. `product-hook-attachments` effect-binds HookService plus Session to the
Agent/SubagentRegistry/MCP call sites. Its durable bridge performs append,
flush and physical readback before model rendering. A pre-minted session
constructor in `heycode-session` lets root create these adapters before composition;
the default production factory now selects this bridge and mounts
`product-hook-attachments` after Agent/subagents and before TUI.

U17 now treats the session post-commit bus as the transcript authority. The
TUI plugin owns that listener as a Context effect, caps its pending lane at
1,024 events and rebuilds from durable history after lag. Replay and live
delivery use the same reducer, and parallel client tools correlate by durable `CallId`
instead of one "latest tool" slot. Normalized provider-tool calls/results,
aggregate usage, public citations, portable/native compaction, provider-state
identity, route/runtime changes, plan/goal/workflow state and typed untrusted
results have matching full and flat cards. Opaque provider-state JSON and
provider-tool input never enter the renderer.

U21 adds a per-AppState transcript height index and a content/width/reasoning/
theme-keyed rendered-fragment cache capped at 256 entries. Redraw parses and
highlights only visible items, and appending extends the cheap index rather
than rebuilding it. The focused 100,000-event session gate replays under two
seconds and renders bottom and middle frames under a one-second debug
budget while each frame renders at most forty new item fragments and retains
at most 256.

The scroll offset is counted in *rendered* rows, not indexed rows. The window
walks real rendered items backwards from the newest, so one offset unit moves
the screen by exactly one row across the newest 32 items and follow mode needs
no index at all — an item drawn taller than its budget can neither push the
newest content off the screen nor make an offset a no-op. Offsets older than
that walk jump with the height index, which over-counts on purpose. What the
jump guarantees is monotonicity and top-reachability — the window never moves
back down as the offset grows, and every row stays reachable at whole-item
granularity — not rate: skipping N over-counted bound lines skips fewer than N
rendered rows, so past the walk the window advances less far than asked, about
one row per five offset units on a markdown corpus. `Home` (the `usize::MAX`
sentinel) lands on the oldest row directly.

Concurrent approval dialogs queue. Parallel tool calls each park their own
caller on the approval policy, so the ask plane keeps every request in line
behind the one on screen, shows how many wait in the card and the screen-reader
projection, and answers them one at a time; a policy-side resolution drops its
request from the queue rather than opening a stale dialog. Quit cancels the
turn task and waits a bounded time for it, aborting and detaching a task that
ignores its token rather than holding the terminal open.

CMD09 contributes `/diff`, `/review`, `/copy` and `/mention`. Diff opens the
committed U22 projection, copy requests a bounded base64 OSC 52 write only in a
full terminal, and mention edits only the composer. Those three commands do
not append session/model history. `/review`, `/advisor`, and `/security-review`
are `model_scheduling` commands that invoke the native `reviewer`, `advisor`,
and `security-review` child presets. Each child has an enforced read-only
permission ceiling and a maximum of 32 inference steps. The parent logs the
exact slash-command intent, child task id, result, and terminal outcome; the
normal turn cancellation control cancels the child too. Minimal profiles may
omit the subagent service: the inspection commands remain discoverable with an
unavailable reason, while the rest of the TUI continues to work.

These commands inherit the current native provider/model/effort. To explicitly
choose a stronger route, save an overriding preset with `/agent-config` and set
`config.inference_provider` plus `config.model` (and optional `config.effort`).
There is no implicit provider upgrade. Preset permissions cannot remove the
command's read-only ceiling, and an external child runtime selection is refused.
The separate `/review-runtime` delegated-runtime workflow remains available.

CMD10 contributes `/theme`, `/keymap` and `/vim` alongside `/settings`. The TUI
plugin effect-registers `ui-preferences` and `keymap` Settings namespaces.
Theme and Vim changes preserve the other preference and commit at an exact
revision; keymap edits use the same stale-CAS boundary. The full and flat theme
and keymap modals share focus order, theme arrows preview before Enter commits,
and Vim mode exposes insert/normal state with bounded normal-mode navigation.

The active shell keeps a persistent identity header with the heycode version,
execution owner, model and workspace. Command, model, provider, settings,
permission, profile, session, plugin, MCP and capability browsers reserve rows
next to the composer instead of covering the transcript with centred cards.
Operational status and command hints are separate Settings-backed footer rows;
`header_density`, `footer_status` and `footer_hints` apply live and survive
theme/Vim writes. Short terminals collapse the hint row and the second header
row before giving up transcript content. Full-screen trust, onboarding, secret
and approval decisions remain deliberate foreground dialogs.

U15/CMD06 adds a TUI-owned session browser over the effect-owned
`SessionQueryService`. Pages are ten-row deterministic keyset pages with text,
active/archive, root/fork, status, source, current-cwd and current-runtime
filters. Every row shows safe title/id, source, runtime, provider, cwd, durable
activity, event counts and lineage plus distinct current/latest/archive
markers. A corrupt/unsafe session becomes a visible fixed store error and never
a silently missing row.

Queued `/new`, `/resume`, `/fork`, `/rename`, `/archive`, `/delete` and
`/export` commands share one bounded shell bridge. New/resume/fork publish a
typed `RecomposeSession` outcome only after the lower service created or
validated the target; the TUI does not load a second world. Rename appends
through the live current-session owner or the closed-session service. Archive
uses the recoverable in-place marker. Delete always opens a cancel-default
confirmation, while the lower service independently refuses current, open and
ancestor sessions before moving a leaf to recoverable trash. Export produces a
lineage-complete JSONL bundle, bounded Markdown artifact, or structurally
redacted support trace via explicit `jsonl|markdown|support` selection.

The TUI plugin declares `session-query`, registers the session panel and all
seven commands as Context effects, and retains no lifecycle task or detached
wait. The composition root translates `RecomposeSession` into the exact
committed `<sessions-root>/<id>/session.jsonl`, removes older resume selectors,
preserves all other arguments, shuts down the world/runtime and re-enters
startup. Config schema v23 inserts `session-query-jsonl` before historical
exact TUI profiles.

Q03 has a crate-local reusable journey recorder in the integration-test support
layer. Each step is explicitly classified as a host action, terminal event, or
live UI event; the step is applied to the production `AppState` reducer and
captures the production screen-reader projection. Trust, first-run setup,
command-palette, MCP-management, and provider/runtime-picker journeys use fixed
typed fixtures with no clock, terminal timing, provider request, child process,
or network dependency. Checked-in exact frames pin modal priority, focus,
catalog/panel facts, and keyboard movement.

U19's product-neutral lower boundary is `TuiDisplayMode::ScreenReader` plus
`TuiHandle::run_screen_reader`. It forces colorless, motionless flat rendering,
uses the same live transcript/status/dialog/command/panel state and the same key
router as the full-screen UI, suppresses duplicate unchanged frames, strips
terminal controls from state-derived text, and never writes alternate-screen or
cursor-control escape sequences. A lifecycle guard restores raw/full terminal
state on every return path; flat mode emits no screen-enter/screen-leave bytes.
Automatic `TERM=dumb` handling uses the same flat writer instead of painting a
Crossterm frame that would still require cursor addressing.

The composition root now exposes `--screen-reader` only for interactive TUI
startup and maps it to this exact mode. Connection, profile, session and trust
recomposition preserve the flag with every other untouched original argument.
Headless, setup, ACP, doctor, MCP and plugin modes reject it rather than
silently ignoring an accessibility request. A future persisted preference can
reuse the same product-neutral choice without adding a second renderer.

## Slash commands and the palette

The composer owns command text. Typing `/` into an empty composer types the
slash and opens the palette as a view over the first token; Ctrl+P opens the
same view, setting a non-slash draft aside and restoring it on Esc. Keys go to
the composer and the palette follows; only an exact, prefix or substring match
on a command *id* is preselected — description, source, subsequence and typo
rows are listed but run only after the user arrows to them, so `/memory` can
never run `/compact`.

One Enter runs what was typed. An exact id, or any id followed by arguments,
runs as written (`/model deepseek-v4-pro`, `/title Fix the bug`,
`/init apply <token>`); a prefix of an argument-free command completes and runs;
a prefix of an argument-taking command completes to `/id ` for the user to
finish; an unknown command reports `unknown command … — try /help` and is never
sent to the model; an unavailable one reports its reason. Esc closes the
palette and keeps the text. Session commands that recompose (`/new`,
`/resume`, `/fork`) act in the same loop iteration and the new transcript opens
with `new session …`, `resumed session … · N earlier turn(s)` or
`forked from session … at event N`, so no keystroke is swallowed and no
transition is silent.

## Context meter and profile picker

The footer shows a circular context indicator with used/capacity tokens and
percentage when both values are known. Native estimates and delegated
current-request measurements remain separate from cumulative billing usage;
missing authoritative capacity stays unknown. Past `context_warn_ratio` (the
compaction threshold minus a margin, wired from config) the meter is prefixed
`!` and the flat frame says `— compaction soon`. The profile picker always offers a
`built-in (no profile)` row, marks the current one `(current)`, and selecting
the current row is a no-op close; a switch resumes the session on screen.

## The composer

Prompt history is durable: `prompt_history::PromptHistoryStore` keeps one JSON
string per line under `$HEYCODE_HOME/history`, owner-only and bounded to 500
entries, so Up reaches yesterday's prompt and not just this process's. An
unreadable line costs only itself.

`--screen-reader` draws flat because the user asked for flat, not because the
terminal cannot address a cursor, so `Chrome::input()` records what the screen
may ask of that terminal: a requested flat frame still enables bracketed paste
(and nothing else), while a genuinely `dumb` terminal is still written zero
bytes. Without it a screen-reader user's pasted block arrived as keystrokes
whose first newline sent line one.

The full-screen shell enables bracketed paste and the kitty keyboard
enhancement flag on entry (and undoes both on exit), so a pasted block arrives
as one `Event::Paste` that lands in the composer as a multi-line draft — nothing
is sent until Enter — and Shift+Enter is distinct where the terminal supports
it. Flat mode writes no control bytes and therefore has neither. The input area
grows with the draft, one row per (wrapped) line up to ten, so every pasted
line is visible. A line break is inserted by Alt+Enter (`insert-newline`,
rebindable), Shift+Enter where reported, or Claude Code's `\` at the end of a
line followed by Enter.

Ctrl+C follows Claude Code: with a draft it clears the draft; with an empty
composer it arms `press Ctrl+C again to exit` (shown in the input area and the
flat frame) and only a second press quits; any other key disarms it. Up on the
first line and Down on the last walk the session's sent prompts, restoring the
in-progress draft past the newest entry. Esc closes an open side panel before
it means "interrupt", and Esc in the `/connect` wizard goes back a step or
closes it — it exits heycode only from the first-run Welcome.

## Native steering and follow-up

During native active work the live keymap gives Enter to Steer, the persisted
`queue-follow-up` action (Tab by default) to FollowUp, and Esc only to the
current cancellation owner. Idle Enter still sends a new turn and idle Tab
remains textarea input. Slash commands keep their U11 scheduling path and must
use Enter, so Tab cannot accidentally turn a human-plane command into model
text.

Composer text is appended to the durable Agent inbox and cleared only after
that commit. Safe narration names the delivery but never repeats the text;
transcript/model publication happens only when Agent claims it into the exact
`user/message`. Both renderers show next-turn/next-step counts and active key
meaning. One settlement Wake starts one joined follow-up task before queued
commands; another wake is required for another message. Resume seeds pending
counts from JSONL. If the Agent settles between key routing and append, an idle
steer is cancel-recorded and requeued as a follow-up instead of being stranded.

Delegated runtime controls remain visibly unavailable here until the stable
app-server owns their provider-native steer/follow-up methods. The shell never
falls back to the composed native Agent, which would target another session.

Full-screen Markdown is also a terminal-security boundary. Assistant text is
normalized before pulldown-cmark/syntect: LF and CRLF keep their structural
meaning, tabs become spaces, and every other Unicode control becomes a visible
replacement character. ESC/CSI/OSC bytes therefore cannot become ratatui cell
symbols or terminal control sequences. The checked-in fuzz corpus retains ANSI
colour and OSC 8 hyperlink payloads; both the focused unit regression and the
public draw-path libFuzzer target require every rendered cell to be
control-free. Flat mode keeps its independent final-output sanitizer.

Focused verification:

```sh
cargo clippy -p heycode-tui --all-targets -- -D warnings
cargo test -p heycode-tui
```

## Connection setup update — 2026-09-05

Onboarding offers four direct choices: subscription, local model, API provider and managed cloud. Available titles stay bright and bold, descriptions are indented, and selected titles use an accent background. Scrolling keeps the selected option and footer visible. Subscriptions use runtime-owned account/catalog discovery. API choices bind authorization flows to provider credential references. Session open waits for connection and workspace trust.

Connection lists accept typing and paste as search input, show the query and empty results, and consume those events before the composer.

The local/provider lists use connection metadata independently of authorization flows. Configured credentials proceed to model discovery; local failures keep provider-owned setup help visible. Saved connection repair retains the provider identity.

Catalog authorization failures, including stale-view warnings, return to the named connection repair page before model selection. Network failures retain their separate fallback behavior. Repair opened from `/connect` can be dismissed back to the running session.

Local connection setup edits the server URL before discovery and persists it only with the chosen model. Changed input invalidates in-flight discovery results. LM Studio and Ollama support optional masked API-key entry.

LM Studio endpoint keys now use a masked, operation-scoped flow. Server validation precedes file credential commit; the returned model snapshot and a fresh credential reference are staged together on model selection. Failed validation preserves previous credentials and the active catalog. Unsupported endpoint authentication is reported before asking for a key.

The full-screen PTY key scenario drives the built CLI against an isolated LM Studio-shaped fixture and verifies masked entry, model-list rendering and file credential commit without terminal secret exposure.

The onboarding model picker respects provider-owned adapter model limits before displaying discovered choices.

Managed-cloud profiles render their ordered provider-owned coordinate fields through the same modal and key router. Amazon Bedrock exposes one region field, live no-cache discovery, optional masked credential entry, and atomic region/model/reference staging. Full and screen-reader renderers name the current field and its position; no coordinate or key enters the composer.

A production-binary pseudo-terminal regression drives real arrow keys and bracketed paste from Welcome through the managed-cloud hierarchy into Bedrock's region field. The fixture-backed production-composition test separately proves that the resulting saved tuple reaches authorization, status, catalog admission and Converse inference.

Integration keeps exactly three welcome choices. Cloud and API profiles share Select a provider; cloud PTY checks reach their forms through provider search.

## Compact conversation chrome

The default header combines an animated orange cat, brand, active backend model,
effort and workspace. The cat blinks and twitches its tail at idle, then glances
around while working;
`/settings` can hide it, and terminal animation preferences are respected.
User turns have a subtle full-width background, while assistant turns use a
bullet and hanging indent. The composer has an inline prompt and continuation
rail for multiline drafts. Two compact footer rows show the green branch/open PR, purple concrete model,
cyan measured input/output counts and context meter, then the approval mode and
applicable shortcuts. Effort sits above the composer on the right.
Runtime-link and health diagnostics remain in diagnostic/accessibility views.

`/permissions full_access` allows actions without prompting;
`/permissions default` returns to asking each time. Task tools update the visible task summary.
Typing `/exit` or the unknown `/exist` never executes it before
Enter; an unknown command remains an error rather than exiting.

The reported concrete model ID takes precedence over a marketing label or alias
in the header and footer. Sparse effort/catalog refreshes preserve that identity;
a different model selection clears it. Current-request context evidence can
also carry the resolved model without changing the saved control selector.

Activity appears immediately above the composer, with phase-specific working,
reasoning, responding, tool and user-wait labels plus monotonic elapsed time.
Model activity includes the effective effort (or `default` when unspecified).
Before a provider signal it says `Working… (awaiting model · … effort)`;
observed reasoning switches to `Thinking… (… effort)`. Live reasoning cards
preview the latest two rows of provider-supplied text; Ctrl+R or clicking the
header expands the full available text. Settled cards collapse to their header.
Encrypted-only reasoning shows activity with a readable-text-unavailable note;
opaque provider state is never rendered as thinking text.
Effort choices use a vertical dropdown; Up/Down selects and Enter confirms.
The composer no longer repeats the effort already visible in the shell.

The shared `ask_user_question` tool opens a foreground question card with a short
header, prompt, labelled choices and optional descriptions. Up/Down selects a
choice; typing or pasting enters a custom Other answer. Enter submits and Esc
cancels explicitly. The model waits for this response even in Full access mode.
Native and delegated host-tool calls use the same question service, which
serializes concurrent questions and binds replies to the owning surface.

Shift+Tab cycles Full access, Accepted edits, Default and Plan using
the same command owner as `/permissions`. The draft remains intact and the
footer updates only after the policy commits. The Plan review also accepts Shift+Tab so the user can leave Plan directly.
Other foreground dialogs keep ownership of their keyboard input.


`/logout` retires the active backend, cancels its inference, clears the saved
connection and returns directly to the full Welcome screen. A durable setup
requirement prevents environment credentials, project defaults or vendor login
state from silently reconnecting after restart. Explicit connection setup starts
a fresh conversation. Native logout deletes the exact saved writable credential,
including optional local-server keys; a cleanup failure is shown in Welcome
while the disconnected state remains enforced. External vendor logins are owned
by their respective CLIs and remain available for explicit reconnection.


The Permissions picker contains four choices: Full access, Accepted edits,
Default and Plan. The agent leaves Plan through full-document review; the user
can leave directly with Shift+Tab, the picker, or `/plan off`. Full access asks
no permission. Accepted edits automatically permits native reads and file edits;
shell commands and other tools still ask. Default asks for every call and offers
Accept, Accept + allow edits this session, and Reject. The second choice commits
Accepted edits before allowing the pending call. Other tools in Accepted edits
can receive an explicit grant for identical calls; changed inputs ask again.
Denial and cancellation never grant access. Returning to Default asks each time.
The picker and Shift+Tab use the same live policy owner.
Sandbox restrictions remain under `/sandbox`. Configuration schema 30 still
interprets legacy unconditional `auto` settings as `full_access`.

Credential setup starts with an empty field and a separate hint. Keys longer than
six characters show the first five and final character, with the middle masked;
short values stay masked so the whole key is never exposed. Long inputs keep the
final character visible. Provider-owned validation runs before saving new keys
and before discovering models with saved keys. A rejected key reopens an empty
field with safe feedback; cancellation preserves the previous saved credential.
Public catalogs, cached catalogs and fallback defaults are not authentication
proof. Network and catalog failures keep setup open for retry.

Plan is the fourth permission choice for native sessions. Shift+Tab reaches it
from Default and leaves it directly, including during a pending review. Agent
requests to leave Plan require a full-document review. The dedicated review
uses the available screen for scrollable Markdown and keeps three explicit
choices visible: implement with Accepted edits, implement with Default, or stay
in Plan and revise. Up/Down, PageUp/PageDown, Home/End scroll the document;
Tab/Left/Right select a choice; Enter confirms. The initial choice is No.
Typing/pasting enters feedback and selects No; Escape remains read-only.
`/plan review` reopens a saved proposal after feedback or resume.
## Return recaps, asides and rewind

Human dictation is available with `/voice start`, `/voice stop`, `/voice cancel`
and `/voice status`. When a draft already exists, use Ctrl+P to run the command
while preserving its text and cursor. A visible composer caption reports
starting, recording, stopping and local transcription. The result is inserted
at the saved cursor and is **never submitted**. If the draft changed during
dictation, `/voice insert` explicitly inserts the waiting transcript into the
current draft; `/voice cancel` discards it. Opening a child task or switching
sessions cancels the operation, and stale results cannot change another draft.

On macOS, the fixed AVFoundation helper uses `/usr/bin/swift` and the installed
Command Line Tools. Only `/voice start` can open the microphone or request OS
permission. `/voice status` queries permission without recording. Swift uses
an owned private temporary module cache under the existing sandbox policy;
there is no less-restricted execution fallback. Other platforms need an
explicit `HEYCODE_VOICE_CAPTURE_COMMAND` literal-argv helper. A configured local
recognizer/model is required on every platform through `HEYCODE_STT_COMMAND`.
No provider audio support or paid model call is required.

Native sessions expose `/recap [on|off]`, `/btw <question>`, `/output-style`,
`/questions` and `/answer`. Full-screen focus reporting triggers a return recap
after three completed turns and three idle minutes, at most once for a given
completed turn. `/recap off` persists the opt-out; manual recap remains available.
Generation starts on return and remains asynchronous to terminal input.

To ask an aside with an unfinished draft, press Ctrl+P and use `/btw`; the full
text area and cursor return after submission, including when `/btw` was first
completed from its prefix. The main turn continues.

`/rewind` lists prompt checkpoints; `/rewind <turn>` restores the conversation
before that zero-based turn, and `/rewind <turn> files` also restores confirmed
native text edits if their current contents match. Rewind creates a recoverable
fork and restores the selected prompt into the composer without submitting it.
A changed file causes refusal. Recovery postimages stay beside restored files.

The persistent Tasks strip expands with Ctrl+T (the configurable `toggle-tasks`
action) or a mouse click. Arrow keys and Enter open an actual child, tool, job,
or team. Child views retain separate drafts; Esc restores the full parent
editor. Enter sends to the selected native child or interactive terminal.
Alt+I interrupts; Alt+X closes a settled child; Alt+B moves an eligible running
execution into the background without restarting it. Alt+O switches process
output streams; PgUp/PgDn read retained output and Ctrl+End follows live output.
Alt+M shows scrollable owner, session, runtime, usage, tool and changed-path
facts. Missing telemetry is labelled unavailable. Team details show committed
dependencies, results and delivery/claim state. Higher-priority dialogs keep
exclusive input ownership. Interrupted continuable children remain resumable.

`/agents` checks each registered provider's current readiness independently of
its declared fork, continuation and interruption support. It shows `Ready`,
`NeedsAuthentication`, `Unavailable`, or `Unknown` with a reason. An unsupported,
failed, cancelled or timed-out probe never becomes a false readiness claim.
Use `r` to recheck, `c` to cancel checks, and Esc to close. Checks have a two-second
limit, run at most four concurrently and cover at most 32 providers per opening;
closing or replacing the panel cancels its probes. No check signs in or starts a
child. The selected provider's capability evidence and setup guidance wrap in
the visual panel and appear in the screen-reader output.

Consecutive successful reads, searches, edits and shell calls appear as quiet
activity summaries between assistant messages: `Read 2 files`, `Searched for 2
patterns`, or `Ran 2 shell commands`. Click a summary to expand the original
calls, reasoning and retained output. Ordinary running calls use the single live
activity line and enter the transcript when settled. Failures and pending
approvals stay visible as individual cards. Alt+Up/Down focuses a group or card and Enter/Space
toggles it. Routine mode and runtime
messages are available in session diagnostics. The task strip appears only when
tasks exist; Ctrl+T still opens an empty task list. Foreground shell output is
shown once as the tool result.

The composer uses plain cursor-line styling without underlining. Standalone
`ultracode` and `workflow` keywords receive a purple highlight and a turn-scoped
workflow hint. Successfully retrieved command output joins the original tool
card by job ID; expansion retains the original metadata and retrieved pages.
Unmatched retrievals and failures remain visible as compact Task output cards.

Todo updates are labelled `Plan updated` with a completion count, separate from
the task execution strip. The plan footer retains the current checklist. File
reads show a line-count summary until expanded; expanded output preserves tab
spacing. Failed searches show their error without a match count. Shell previews
show the last two nonempty output lines, and disclosure hints appear on focused
cards. Completed/cancelled tool and job records remain in Ctrl+T history but no
longer inflate the collapsed task strip.
