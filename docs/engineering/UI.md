# TUI, onboarding and command specification

## Design direction

The visual language follows Claude Code: restrained terminal-native layout, readable transcript, compact status, clear tool cards and low-noise motion. Interaction design borrows Codex's searchable slash commands, permission/status visibility and MCP startup feedback, plus OpenCode's always-visible provider/model/agent identity and command palette.

The interface must never hide effective provider, model, reasoning level, workspace, permission mode or health state.

## UI architecture

The TUI is a plugin host, not a monolithic renderer. `UiRegistry` exposes typed slots:

- Startup gate.
- Welcome card.
- Transcript item renderer.
- Tool renderer keyed by logical tool name.
- Composer prefix and footer.
- Status-line item.
- Command palette entry.
- Modal/dialog.
- Settings section.
- Notification/toast.
- Side panel.

Contributions return disposers. A plugin cannot reach the terminal backend directly; it submits state and pure render descriptions to the UI service.

## Global layout

```text
┌ transcript / welcome / setup ───────────────────────────────────────┐
│                                                                    │
│ ❯ user                                                             │
│ assistant markdown                                                 │
│ ⏺ read(src/main.rs)                                                │
│   ⎿ 120 lines                                                      │
│                                                                    │
├ optional inline dialog or side panel ──────────────────────────────┤
│                                                                    │
╭─ ❯ ────────────────────────────────────────────────────────────────╮
│ Ask anything…                                                      │
╰────────────────────────────────────────────────────────────────────╯
 provider · model · effort · permission · cwd · context · status
```

The transcript owns vertical expansion. The composer is two to eight rows depending on content. Dialogs use a fixed, bounded region or centered overlay and never corrupt transcript history.

Implemented U22 registers Diff, Jobs and Agents as exact `UiSlot::SidePanel`
contributions. Persisted action `cycle-side-panel` ships on Ctrl+B and cycles
Diff → Jobs → Agents → closed. Diff derives from committed edit/write results,
Jobs from the effect-owned JobRegistry and Agents from live provider/preset
descriptors without cross-owner child ids. Full and screen-reader modes consume
the same bounded control-free snapshots; trust, secret, onboarding, approval,
palette and picker modals remain higher priority.

## Visual tokens

| Token | Default |
|---|---|
| Accent | `#D97757` |
| Success | `#7FB069` |
| Error | `#E06666` |
| Warning | `#D9A05B` |
| Text | `#E8E4DC` |
| Dim | `#8A857C` |
| Border | `#3A362F` |
| Background | terminal default |

Markers remain `❯`, `⏺`, `⎿`, and `✻`. Color is never the only state indicator.

Animations run only while work is active. Idle mode has no periodic redraw timer. Respect reduced-motion and screen-reader modes.

## Startup state machine

```text
Boot
 ├─ config migration required → Migration preview
 ├─ workspace untrusted       → Trust workspace
 ├─ no usable runtime         → Welcome / Connect
 ├─ provider unhealthy        → Repair connection
 └─ ready                     → Welcome / Composer
```

Startup renders before provider/MCP initialization finishes. Background services publish progress into the welcome/status area.

### Workspace trust

The first launch in an unknown workspace shows:

- Canonical workspace path and Git root.
- Whether project config, plugins, hooks, MCP servers and instruction files were detected.
- Effective filesystem, shell and network permissions.
- Choices: trust once, trust this workspace, open read-only, exit.
- Link/command for the security guide and `heycode doctor --trust` details.

Project-level executable plugins, hooks and MCP do not load until trust is granted. Reading the directory to render the trust summary uses a minimal safe scanner.

Implemented U01 is the security-critical subset: the trust modal has highest
priority over every other surface, defaults to Open restricted, supports
arrows/Tab/Enter/Esc/double-Ctrl+C, consumes a live CAS-bound prompt and blocks
model sends, slash/palette commands, paste and queued promotion. Ready returns
a typed recompose outcome; Exit mutates nothing; stale actions refresh. The
pre-trust session is ephemeral. Broader Git/detected-content/security-guide
explanation above remains DOC06 rather than an implemented claim.

### Migration preview

Configuration migrations show the source file, schema versions, semantic changes and a redacted diff. The user may apply, back up and apply, continue read-only, or exit.

The first migration recognizes old setup-generated `[profile].plugins` snapshots. If the list exactly matches a known historical generated profile, it converts it to the built-in profile rather than preserving an obsolete plugin freeze.

Bootstrap status: before this panel exists, schema v4 startup safely auto-applies typed historical `$HEYCODE_HOME/config.toml` migrations (frozen generated profile, setup-generated retired DeepSeek default, v2 custom-profile `agent-options` dependency and v3 custom-profile `ui` dependency), prints redacted semantic notices and preserves byte-exact backups. Dependency migrations retain every intentional plugin and insert only the prerequisite immediately before its dependent. Explicit/project files remain pending and customized model pins remain unchanged. The panel must consume the same migration-plan types; it must not introduce a second migration implementation.

B10 now exposes that same notice through `heycode doctor [--json]` as the `config-migration` check. The evidence schema contains only version/disposition/path and typed semantic changes; source/rendered TOML bytes and unknown extension values are structurally unavailable. The future panel must render this check rather than reparsing files.

### Welcome and connect

The welcome card contains:

- Product/version and update state.
- Provider/runtime and auth method.
- Model, context window and reasoning selection.
- Workspace and Git branch.
- Permission preset.
- MCP/plugin health summary.
- One relevant tip or migration notice.

If no usable runtime exists, the card becomes the first-run wizard.

Implemented U02 boundary: interactive startup with no usable credential no longer asks the old line prompt. The world composes an inactive/disconnected provider plus the plugin-owned onboarding state, and the TUI blocks the composer behind Welcome. Enter advances to the generic runtime-class page; arrows/Tab select, Esc exits, and double Ctrl+C remains global. Connector/auth/model/permission pages are contributed in U05–U09/S08 rather than hardcoded into this renderer.

Implemented U06/U10 boundary: selecting a runtime class filters the live authorization descriptor catalog into a dynamic method page, with API/router choices restricted to the effective provider so a credential cannot be committed for a different route. Selecting one starts S08 and opens a centered masked input card through `secret-prompt`; keystrokes/paste are capped, stored only in private zeroized UI state and rendered as bullets. Enter validates and commits through registry-owned authoritative readback; safe failures stay on the method page. Success shows Connected with Continue. Continue restores the terminal and requests exact-argument product recomposition rather than exposing the disconnected composer.

Implemented B09 compatibility boundary: the legacy `heycode setup` terminal flow no longer contains a provider/model table. Its restricted setup world projects provider-owned registry name/display/default/credential metadata and model-catalog generations; retired models are filtered, stale warnings stay visible, and a missing provider catalog falls back explicitly to that provider's default while allowing a custom id. It creates no agent/session/tools/TUI/MCP or live inference client. U07 supplies the searchable full-screen picker, while U10 now owns the integrated hot ready transition.

Implemented U03 boundary: `heycode-ui` owns the UI-neutral registry. Plugins contribute validated panel/dialog/status metadata plus a typed opaque handle as a context effect; priority snapshots and exact `ui_slot` inventory are deterministic, and rollback/shutdown removes every handle. The built-in TUI now contributes `panel:transcript`, `dialog:approval` and `status:session`. U04/U05/U07–U14 build interaction/render contracts on these slots rather than adding panel lists to the composition root.

Implemented U14 boundary: the same plugin publishes `settings-ui`, whose default
surface derives typed controls from each registered Settings schema and whose
custom namespace claims are token-owned Context effects. `/settings` is an
immediate TUI command sharing the running shell's panel inbox. The browser opens
from fresh namespace snapshots, renders origin plus live/restart behavior, and
commits only toggle/choice/text/number user-layer edits through the snapshot's
exact revision. A stale CAS stays in the browser with a conflict notice and
fresh rows. Managed, project, secret, unrenderable and custom-delegated fields
remain visible but non-editable with a reason; secret values are structurally
absent from both UI layers. Trust, secret, approval and onboarding surfaces
still preempt the browser.

Implemented CMD04 path (focused acceptance pending): `/mcp`, `/agents` and
`/hooks` share the running shell's one-slot panel inbox and advertise
availability only after their service is attached. Existing owners keep
`/plugins` and `/skills`; their no-argument form emits a validated opaque panel
request on the human-only UI bus, while `/plugins verbose` and `/skill` retain
their existing non-navigation behavior. Production startup attaches the exact
Settings-backed MCP/plugin operations plus live `SkillSet`, subagent and hook
registries before dispatch. Skills/agents/hooks use one bounded read-only view
in full and screen-reader modes; it omits skill bodies, authority-scoped child
ids and hook command/prompt/argument content.

MCP11 uses a separate highest-priority session-scoped surface. Each configured
connection has one router bound to the durable session id; form and URL
elicitation, progress and logs reach full and screen-reader modes through the
same bridge. Closing the TUI cancels every pending elicitation and sends no late
reply. Tool approval is not answered by this surface: MCP13 routes the call
through the exact ordinary Agent policy, and server annotations are absent from
that policy request.

## First-run wizard

The wizard is a TUI plugin and reuses the same dialogs as `/connect`, `/model`, `/permissions` and `/mcp`.

### Step 1 — choose runtime class

```text
How do you want heycode to run?

  Subscription
    ChatGPT / Codex
    Claude

  API or router
    OpenRouter
    DeepSeek
    OpenAI API
    Anthropic API
    MiniMax
    Z.AI / GLM
    Custom endpoint

  Cloud
    Amazon Bedrock
    Google Vertex AI

  Local
    LM Studio
    Ollama
```

Unavailable entries remain visible with a concise prerequisite instead of disappearing.

### Step 2 — authorize

Authorization UI is method-specific:

- API key: masked field, destination host, storage choice and live validation.
- Codex subscription: R04 can now display credential-blind account source/ChatGPT plan and visible model capabilities from the pinned app server. Browser/device-code login, logout, rate limits and primary-session selection remain R06/later UI work.
- Claude subscription: launch or inspect official Claude Code login; never read its token file.
- AWS: choose API key, profile, SSO or ambient chain; test STS/Bedrock access.
- Vertex: choose ADC, service-account helper or ambient workload identity; test project/location.
- LM Studio: probe endpoint, optional bearer token and server version.

The validation call must identify unauthorized, wrong host, expired credential, missing scope, unavailable model and transient network failure separately.

### Step 3 — choose model

The model picker queries the selected catalog and displays:

- Display name and exact id.
- Provider and route.
- Context/output limits.
- Text/image/audio/file inputs.
- Tool calling and parallel tools.
- Reasoning levels.
- Structured output.
- Native search/fetch/code/compaction/cache badges.
- Price, latency and throughput when the provider supplies them.
- Preview/deprecated/retirement status.

Search is fuzzy across id, display name, provider and tags. Filters include tool-capable, image-capable, local, free, fastest, cheapest, largest context and stable only.

Implemented CAT05 substrate: tool/image/reasoning filters match only explicit capability evidence, with Unknown separately queryable; lifecycle offers Selectable and Stable as distinct choices and evaluates retirement against one explicit UI query instant.

Implemented U07 boundary: `/model` with no id opens a centered async picker over the active provider's `CatalogRegistry`. It fuzzy-searches id/display/aliases, highlights current model, excludes effective retirement, cycles selectable/stable/tools/reasoning filters with Tab, renders lifecycle and tri-state capability badges, distinguishes live/fresh-cache/stale-fallback/error, and forces refresh with Ctrl+R. Esc cancels the caller wait; Enter updates the live route only from a proven row. Persisted selection and cross-provider opaque-state policy remain CMD02/C14.

Implemented U08 boundary: `/provider` with no id opens one centered picker over live inference-provider profiles and AgentRuntime descriptors. Rows carry loud `INFERENCE API`, `NATIVE AGENT` or `DELEGATED AGENT` badges, current markers, safe defaults/capabilities, fuzzy search and All/Inference/Agent filters. Registered inference rows choose their provider-owned default model; current native runtime is a no-op. Delegated/non-current native rows remain visible with a primary-bridge prerequisite and cannot mutate status. Runtime is shown separately from provider/model in the status line. CMD02 now owns durable selection.

Implemented CMD02 boundary: provider/model picker Enter and explicit `/provider <id>`/`/model <id>` call the same `RoutingService`; Settings CAS commits `[settings.routing]` before welcome/status/Agent update, and reload/restart restores it. `/connect` reopens the U06 runtime-class/authorization wizard or runs one exact flow target. `/logout [provider]` deletes only the uniquely mapped authoritative writable credential. `/effort` is discoverable with an unavailable reason until the selected active adapter exposes exact consumed levels. Built-in auth commands are a separate optional plugin, so a minimal custom TUI profile does not silently gain credential flows.

U16/CMD07 neutral command surface is now live. `/context` shows each request
contributor as exact/estimated/uncounted with refusals, context bounds,
published-price cost or Unknown, explicit unavailable detailed cache facts and
live compaction strategies. `/usage` shows durable session lower bounds/routes/
outcomes/cost completeness with the newest 20 turns. `/compact list` is
read-only; `/compact <strategy> [keep]` selects a composed row. A richer cache/
edit inspector remains active until those facts are durable across restart.

Implemented K07 boundary: queued `/profile [name]` is contributed by the TUI but reads the config-owned effect service. Empty args open a centered sorted picker and highlight the current profile; a direct name follows the same validation. Trust, masked secret, onboarding and approval surfaces preempt and close it. Enter returns a typed recomposition outcome rather than mutating the live Context. The shell settles every owned task and restores the terminal; the CLI then preserves all original arguments except the replaced profile pair and rebuilds through the normal startup loader.

Implemented MCP12 result boundary: live and replay tool cards receive the same
durable schema-v1 rich metadata after media commits through ATT01. Raw image,
audio and embedded blob bytes never enter the frame model; cards can render
ordered block/link/structured metadata and safe attachment facts. MCP-originated
results show `UNTRUSTED MCP SERVER CONTENT`, distinct from Web, and remote
`isError` drives the card's failure state without discarding rich blocks.

Implemented E08 result boundary: `lsp_definition`, `lsp_references` and
`lsp_diagnostics` use the same durable tool-result marker with source LSP.
Live and replay cards render `UNTRUSTED LANGUAGE SERVER CONTENT · data, not
instructions`, distinct from Web and MCP. Large results show E06's complete
retained-output preview rather than an independently wrapped/truncated body;
`lsp_servers` contains only validated host configuration ids/languages and is
not marked as server-authored content.

Selection writes a provider/model reference, not a copied model descriptor. The live descriptor is cached separately with a revision and timestamp.

Implemented CAT04/CMD02 storage boundary: `[llm].provider` + `[llm].model` are the composition base; `[settings.routing]` holds the user override as ids only. Safe catalog generations persist independently in schema-v1 `$HEYCODE_HOME/cache/models.json`. The UI must never edit that cache as selection state or imply a stale cached descriptor was newly chosen by the user.

### Step 4 — permissions

Implemented U09 subset uses the authoritative `SandboxCapabilityReport` and always shows three filesystem modes:

- Full Access: host read/write/network, no active wrapper.
- Read Only: host filesystem broadly readable (backend virtual mounts may differ), only required device sinks writable.
- Workspace Write: the same broad read scope, workspace + backend temp writable; temp isolation is not implied.

Each row shows current/effective mode, active vs available backend, exact read/write scope and whether networking is host-visible or isolated. Unsupported rows remain visible with their evidence gap but cannot be selected. Selecting the current row is a no-op; selecting another supported row produces the exact `--set sandbox.mode=...` restart prerequisite and does not fake a live change. U10 intentionally accepts the already-effective selectable row for the first composer; Settings-backed hot policy and custom approval/capability presets remain U13/S08 work.

### Implemented first-run completion boundary

U10 treats provider-owned defaults as a valid explicit starting decision rather than forcing every optional picker before the first prompt. The effective provider comes from the composed route, its authorization flow owns the exact configured credential reference, its registered profile owns the default model, and the live sandbox report owns the effective permission row. `/provider`, `/model` and `/permissions` remain immediately available for changes.

After authorization returns a receipt, the completion page publishes only after credential write, validation-cache record and authoritative safe readback. Continue returns `RecomposeConnection`; the CLI shuts down the old Context, drops the old Tokio runtime and restarts with the byte-equivalent original argument vector. The new world re-runs presence/validation against the configured reference and must compose with inactive onboarding before the composer is visible.

Terminal close, explicit exit, event-stream failure and any fallible loop branch cancel operation tokens before return. Authorization, admitted turn, command, doctor and model-picker waiter handles are then joined; non-model command tasks are aborted and joined, while model-scheduling work is cancelled through the Agent so durable settlement is preserved. No credential commit or child UI operation may outlive the screen.

### Step 5 — integrations

Offer:

- Import non-secret MCP/plugin metadata from Codex, Claude Code or OpenCode.
- Add recommended documentation MCP.
- Skip for now.

Imported entries are previews. Credentials remain in the source product unless the user runs a new authorization flow for heycode.

### Step 6 — health summary

The final page shows green/warning/error rows for runtime auth, model catalog, test request, sandbox, plugins and MCP. “Start coding” is enabled when one runtime and model path is healthy; optional integrations may remain warnings.

## Command system

Implemented CMD01 boundary: command discovery now comes from `CommandRegistry::catalog()`. Every row carries validated structured arguments, immediate/queued/interrupting/model-scheduling timing, owning plugin, optional shortcut and available/unavailable state with a visible reason. `/help` renders the same synopsis/description rows. U04 filters/renders this catalog but maintains no command names, descriptions, sources or usage strings of its own.

Implemented U04 boundary: typing `/` into an empty composer or pressing Ctrl+P opens the same centered command palette. It fuzzy-ranks id/description/source with exact/prefix/substring/subsequence/typo handling, renders synopsis/source/timing/shortcut/unavailable reason, wraps arrow selection and inserts the selected available command into the composer. Esc closes without changing input. Masked secret, onboarding and approval dialogs always preempt/close it.

Implemented U05 boundary: when the transcript is empty, the TUI renders a rounded welcome/status card sourced from the live native agent runtime id, current provider/model selection, composed approval policy kind, canonical workspace and S11 doctor registry. Health visibly transitions from checking to healthy/warning/unhealthy/unavailable with counts. The card disappears once conversation content exists, while provider/model, permission and health remain in the one-line status bar. The doctor task is cancellation-owned on UI exit.

Implemented U11/A04 boundary: while an agent turn is active, immediate commands run in their own joined task without blocking the terminal; queued and model-scheduling commands announce a safe synopsis and retain exact private text FIFO until all active work settles. Interrupting commands open a centered cancel-default confirmation, restore the composer unchanged on cancel, and queue only after explicit interruption. Dynamic availability/timing is checked again at dispatch. Arguments never enter scheduling narration. Esc/Ctrl+C/confirmed interruption use one persistent handler and a per-spawn caller token, so an aborted native turn can be followed safely by another.

Implemented A05/U18 native boundary: while native work is active, Enter commits
a Steer to the next-step inbox, the rebindable `queue-follow-up` action (Tab by
default) commits FollowUp to the next-turn inbox, and Esc only interrupts.
Slash commands continue through U11 on Enter. Queue text is not narrated and
does not render as a user message until Agent's atomic claim commits it. Both
renderers show exact next-turn/next-step counts and the active key meanings.
One settlement Wake starts one joined follow-up before queued commands; resume
seeds pending state, and a turn-settlement race converts an idle steer into a
cancel-recorded follow-up. Delegated runtimes retain the input and show the
control unavailable until their app-server operation bridge exists; the TUI
never sends it to a different native session.

Implemented CMD05 boundary: `/init` is contributed by the `init` plugin and classified Queued. `/init` or `/init preview` renders a bounded create/append/refresh diff and copyable proposal token while changing nothing. `/init apply <token>` rechecks the exact proposal and atomically writes only the versioned heycode-managed section. Existing project instructions outside the markers are byte-preserved; stale tokens, unsafe targets and malformed markers remain visible errors. No absolute workspace path or existing instruction content is echoed, and neither phase becomes model history.

Implemented CMD03 boundary: plugin `status` contributes immediate `/status`, `/doctor`, `/permissions` and `/sandbox`. They read the live Agent, DoctorRegistry and SandboxService only, reject arguments without echoing them, and publish human-plane `UiEvent`s rather than session/model messages. Permission/sandbox commands open the U09 picker with the same report used for text diagnostics; host networking is explicitly “not isolated.”

Implemented WEB04 visibility boundary: optional plugin `status-web` explicitly injects the web registry and contributes immediate `/web`. It lists every registered provider's search/fetch capability and local availability, the configured-or-automatic selection for each operation, and bounded canonical allow/block domains. Keeping it separate from `status` prevents an optional web service from becoming a hidden order-sensitive dependency.

Typing `/` at the beginning of an empty or current composer opens a fuzzy menu immediately. Ctrl+P opens the same command palette without inserting `/`. Commands show name, description, source plugin, shortcut and availability reason.

Command matching tolerates subsequences and minor typos, while exact names rank first. Plugin commands are namespaced when a collision is possible.

Commands declare execution behavior:

- Immediate: status and dialogs can run while an agent turn continues.
- Queued: executes after the active turn.
- Interrupting: requires confirmation and cancels the active turn.
- Model-scheduling: command performs a durable domain change and optionally sends logged input to the agent.

Implemented U15/CMD06: the TUI owns a ten-row keyset session panel over the
effect-owned JSONL query service. Search plus storage/lineage/status/source/cwd/
runtime filters retain visible current/latest/archive markers and fail the page
on a corrupt store row rather than skipping it. Queued `/new`, `/resume`,
`/fork`, `/rename`, `/archive`, `/delete` and `/export` dispatch through one
typed bridge. Delete opens a cancel-default confirmation and the lower service
still independently refuses current/open/ancestor targets. New/resume/fork
return a typed recomposition only after the exact durable id exists.

### Required built-in commands

| Command | Behavior |
|---|---|
| `/help` | Command browser and shortcuts |
| `/connect` | Add, repair, switch or remove provider/runtime authorization |
| `/logout` | Remove selected heycode-managed auth or invoke official runtime logout |
| `/provider` | Search configured providers/runtimes |
| `/model` | Live model picker and reasoning variant |
| `/effort` | Reasoning level picker when supported |
| `/status` | Runtime, provider, model, permissions, context, rate limits and health |
| `/web` | Registered search/fetch providers, effective selections and domain policy |
| `/doctor` | Full diagnostics with actionable repairs |
| `/init` | Generate or improve `AGENTS.md` after preview |
| `/permissions` | Permission/sandbox dialog |
| `/sandbox` | Backend and writable-root diagnostics |
| `/mcp` | Server list, tools/resources/prompts/auth and reconnect controls |
| `/plugins` | Installed/available plugins and enablement |
| `/skills` | Search and invoke skills |
| `/agents` | Agent definitions and live subagent threads |
| `/hooks` | Hook inventory, source and trust state |
| `/plan` | Enter plan mode, optionally with logged prompt |
| `/goal` | Create/view/edit/pause/resume/clear objective |
| `/compact` | Strategy-aware manual compaction |
| `/context` | Context contributors and token budget |
| `/usage` | Tokens, cache, cost, rate limits and provider attribution |
| `/tasks` | Background jobs and subagents |
| `/ps` | Persistent terminal/process sessions |
| `/stop` | Stop selected or all background operations |
| `/diff` | Working tree diff viewer |
| `/review` | Review current changes with selectable runtime/model |
| `/mention` | Attach file/session/reference |
| `/resume` | Session picker |
| `/fork` | Fork current or saved session |
| `/new` | Start a named session |
| `/rename` | Rename session |
| `/archive` | Recoverable archive |
| `/delete` | Recoverable leaf trash move with confirmation |
| `/export` | `jsonl` lineage bundle, bounded `markdown`, or structural redacted `support` trace |
| `/copy` | Copy last completed answer |
| `/theme` | Theme preview and persistence |
| `/keymap` | Shortcut browser/editor |
| `/vim` | Toggle composer Vim mode |
| `/settings` | Full settings browser |
| `/feedback` | Generate redacted diagnostic bundle and optional submission |
| `/quit` | Graceful shutdown after active-operation choice |

Commands from absent capabilities are hidden from the default list only when showing them would be misleading. The palette can show “available after enabling MCP” results under an unavailable section.

## Provider and model switching

Switching provider/model performs a compatibility check against the current session:

- Portable history: switch immediately after committing `model/selection`.
- Provider-specific reasoning state: preserve only when the target adapter accepts it.
- Opaque native compaction state: offer portable re-compaction, fork before switch, or cancel.
- Active turn: queue the switch unless the runtime supports immediate model change.
- Delegated runtime: start/fork the appropriate external thread and explain session ownership.

The confirmation lists expected cache reset and any feature changes.

## MCP panel

`/mcp` opens a list with status icons:

- Starting, ready, auth required, reconnecting, failed, disabled.
- Transport and endpoint/command summary.
- Tool/resource/prompt counts.
- Required/optional state.
- Startup and tool timeout.
- Auth method and expiry status without token values.

Actions: add, edit, enable, disable, reconnect, authenticate, logout, inspect tools, inspect resources, inspect prompts, test, view logs, remove.

## Plugin panel

`/plugins` separates installed, available, project and managed plugins. Each entry shows version, source, signature/checksum, requested capabilities, contributions, enablement scope and health.

Install shows an exact preview and trust decision. Updating shows version and permission changes before application. Failed activation retains the prior version and surfaces the error.

## Skills, agents and hooks panels

- Skills show discovered name, description and user-only/model-invocable state
  from the immutable composed service; they do not reopen `SKILL.md`.
- Agents show registered provider descriptors with independent fork,
  continuation and interrupt evidence plus an aggregate live-child count.
  Owner-scoped child ids are not exposed through this global surface.
- Hooks show owner, pre/post phase, lifecycle event, handler kind and
  user/project scope. Executable commands, prompt text, MCP arguments and
  handler results are never panel fields.

## Transcript contracts

Transcript items are derived from session events. Live UI events may animate a committed item but cannot invent durable content.

X04 routes ordinary foreground TUI turns through the composed
`LocalAppClient` and stable app-server v1. User/attachment echoes, assistant and
reasoning deltas, usage and turn settlement come from sequenced protocol
events. Local-only dialogs, status, commands and richer structured tool cards
continue on their plugin UiEvent plane; direct duplicate lifecycle events are
filtered only while an app-client turn is active. The TUI owns/cancels/joins
the one turn task and closes the client before Context teardown.

X05 makes future desktop/IDE dialogs registry clients rather than config-file
editors. The optional control contribution exposes typed authorization,
provider/model, MCP, plugin and settings snapshots/mutations. Authorization
catalog rows are explicitly uninspected until a route-bound flow commits; only
masked prompt metadata crosses `control/event`, while answers travel in the
dedicated request and never return. Model fallback keeps the current id visible
but disables it when unproven, with the provider default separately selectable.
Settings rows whose owners did not attest whole-namespace wire exposure show
only id/revision/timing and remain immutable. CAS conflicts, unavailable flows,
catalog fallback and unsupported control generations stay visible/recoverable;
no dialog needs to discover or edit a host path.

X06 makes those same surfaces consumable without linking the host. The TUI's
`LocalAppClient` is now the generic `heycode-sdk::AppClient` over the host's local
raw-JSON transport, so in-process UI calls traverse the same request/response/
notification validation as Rust embedders. The TypeScript package exposes the
same typed callback stream and controls. `start` and `resume(expected_id)` make
the host-selected session identity explicit; streaming remains bounded and a
concurrent cancel is a separately awaited operation. SDKs do not add a socket or
IDE surface—X07/E08 must supply same-user endpoint authority before a desktop
dialog can connect out of process.

ATT02 activates image composition above ATT01. `/attach <path...>` admits one
explicit image through the effect-bound attachment store; `/attach clear`
clears only staged state. The input frame shows the pending count, duplicate
content ids do not accumulate, and up to sixteen images remain recoverable when
capability preflight refuses a send. Only the post-commit
`UserAttachmentsEcho` clears them and adds durable image rows immediately
before the user text. Resume renders the same filename/MIME/dimensions from
`user/attachments`. Headless uses repeatable `--image`; X03 now accepts ACP v1
image and embedded-resource blocks through the same store/runtime association
and echoes them only after durable turn admission.

ATT03 adds `/document <path...>` and ordered headless `--document`. The same
pending composer can mix images and documents. After commit, transcript rows
name the exact route as `native document` or `locally extracted document`; a
catalog refresh never silently changes that label on replay. PDF/HTML source
metadata is staged, while extracted sends echo the derived `text/plain` record
plus its durable source→selected route. Unsupported native capability is a
normal visible local-extraction choice, not an error or marketing claim.

WEB03 `web_fetch` now returns bounded readable text preceded by an escaped
citeable source link and optional PDF page count. The raw source's final URL,
title, retrieval time, truncation state, page count and content address are
durable in `attachment/added`; U17 still owns a structured citation card rather
than parsing opaque tool text. WEB05 marks both web search and fetch outputs on
the typed Tool result. The settled card shows `UNTRUSTED WEB CONTENT · data,
not instructions`, and replay reads the same marker from `tool/result`.

Required item kinds:

- User message and attachments.
- Assistant reasoning, commentary and final answer phases.
- Native client tool call/result.
- Provider server-tool call/result with citations.
- MCP call/result.
- Shell/terminal execution with retained-output link.
- File diff.
- Approval/question.
- Plan, goal, job and subagent state.
- Compaction checkpoint.
- Error and recovery attempt.
- Provider/model/permission change.

Every tool declares a render intent: generic, terminal, diff, locations, checklist, citations, media, progress or custom registered renderer.

N02 now supplies the durable substrate for provider server-tool UI:
`server-tool/call`, `server-tool/result` and `assistant/citation` project bounded
redacted metadata alongside—not instead of—lossless provider replay state.
U17 still owns transcript cards, citation interaction and renderer selection;
the TUI must not inspect opaque provider blocks directly.

## Composer behavior

- Enter sends; Shift+Enter inserts newline; configurable alternatives supported.
- Paste preserves multiline content and warns before accidentally sending very large content.
- `@` opens file/session/reference search.
- `/` opens commands only at command position.
- `!` optionally opens explicit user shell input, governed by current policy and clearly distinguished from model commands.
- While busy, Enter steers the current turn when supported; Tab queues a follow-up; otherwise the UI says exactly what will happen.
- Esc closes a modal, then interrupts active work, then clears input according to current state; the same key never performs two actions.
- Prompt history is searchable.
- Attachments display validation and upload state.

## Status line

Default compact fields:

```text
provider/model · effort · permission · cwd · context% · activity
```

Configurable fields include agent runtime, Git branch, token counts, cache read/write, cost, rate limit, session id, jobs and MCP warnings. Narrow terminals collapse lower-priority fields rather than wrapping.

## Accessibility

- `--screen-reader` uses flat output, no alternate screen and no animation.
- Every color state has text/glyph equivalent.
- Key actions are available as commands.
- Dialog focus order and selected state are explicit.
- Unicode width and grapheme handling are tested.
- Reduced-motion mode disables spinner animation while preserving status text.
- Terminal capability probing falls back safely for `TERM=dumb` and limited color.

Implemented U19/Q03 boundary: one bounded flat projection reads the production
`AppState` and preserves the same modal priority, focus and keyboard router as
the full renderer. It strips terminal controls from state-derived text,
suppresses unchanged frames and writes no alternate-screen, cursor-control,
color or animation bytes. Automatic `TERM=dumb` and explicit shipping CLI
`--screen-reader` select it; the explicit flag is TUI-only and survives every
connection/profile/session/trust recomposition. A reusable deterministic
journey recorder applies typed host actions, terminal events and UI events to
that production reducer and pins exact trust, setup, command, MCP and
provider/runtime frames without clocks, network, subprocesses or terminal
timing.

## UI performance budgets

- Render immediately from local state; provider/MCP work runs asynchronously.
- No idle tick.
- Coalesce stream redraws to a configurable 30–60 FPS ceiling.
- Virtualize or retain only visible rendered lines for long transcripts.
- Syntax highlighting and Markdown parsing cache by content hash.
- Large tool outputs spill to storage; UI renders a bounded preview.

## UI acceptance checklist

- [x] New workspace trust flow is keyboard-complete and snapshot-tested (Unix persistence; Windows durable trust remains a release blocker).
- [x] First-run works with no config and no environment keys (the user supplies a validated credential through the masked flow; fresh-machine real-provider certification remains Q16).
- [ ] Invalid stored credential is detected before entering a normal chat.
- [x] Typing `/` renders a fuzzy, source-aware command menu.
- [x] `/model` uses live discovery and capability badges.
- [x] `/provider` distinguishes inference and delegated runtimes.
- [ ] `/mcp` exposes connection/auth/tool/resource/prompt status.
- [ ] `/plugins` exposes provenance, permissions and enablement.
- [ ] `/status` reports the effective, not requested, configuration.
- [ ] `/doctor` diagnoses stale profiles and retired models.
- [x] Active-turn command behavior is deterministic and visible.
- [x] `/init` previews a managed AGENTS.md change and requires an exact token before apply.
- [ ] Resume replay matches the original transcript including multi-tool steps.
- [x] Narrow terminal, 256-color, truecolor, screen-reader and `TERM=dumb` frames pass.
- [ ] Idle CPU and redraw metrics meet the ship criteria.

### Hidden audio rendering boundary

ATT04 renders only committed `AttachmentMetadata`: duration, sample rate,
channels and bit depth. Full and screen-reader projections share the same
bounded item and never display content ids, encoded bytes or provider payloads.
The app-server emits `assistant_audio` immediately before turn settlement by
reading the durable session suffix; it does not disguise bytes as text/runtime
notices. Current providers expose no audio choice or capability badge.
