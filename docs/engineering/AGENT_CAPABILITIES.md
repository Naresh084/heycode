# Coding-agent capability plan

## Native loop

The native heycode agent remains the primary loop for inference providers. R01 publishes the typed agent-runtime registry/session contract; A01 now mounts the existing loop as exact runtime `native`, preserving its durable turn/tool behavior behind start/resume/send/cancel/compact/close. U08 later selects native versus delegated runtimes without changing the TUI/session browser/orchestration API.

### Turn lifecycle

```text
turn/start
  claim queued next-turn message + all steering/injected context due now
  assemble prompt and tools
  agent/pre_step
  step/start
  commit admitted model-visible input
  project exact-route inputs from log
  refresh exact model evidence + resolve provider call
  request/header + request/context
  independently re-project/verify
  cancellation/retry-aware provider stream
  assistant chunks/items/message
  schedule tool calls
  tool/call → policy/execution → tool/result
  step/end
  another step when tools or steering are owed
agent/turn_stopping
turn/end
```

First-step rejection still closes a durable turn with no model request. Errors and cancellation always close open step/turn anchors or leave repairable transaction markers.

### Inbox and steering

One inbox supports:

- Follow-up: next turn and wake idle agent.
- Steer: next step, including while current turn is active.
- Inject: next step without waking by itself.

Implemented C06 provides the durable projection: v2-only `agent/inbox/splice` stores bounded typed messages and claim/cancel/replacement settlements. One message id is insert-once across the complete session, including after settlement, compaction and resume. Pending operational state is never folded into provider messages; admission requires a later durable `user/message`. A03 still wires live wake/steer behavior to this projection. TUI behavior tells the user whether Enter steers or queues.

### Cancellation

Implemented A04 gives each turn one identity-owned child token fused with Agent-plugin shutdown and optional caller cancellation. Cancelling an idle agent is a no-op and never poisons later turns. Caller cancellation before admission writes nothing; after admission, partial visible assistant output gets a durable interrupted anchor before return. Started tools drain or settle according to their operation contract.

## Tool system

### Registry

Tools are scoped, insertion-ordered definitions with:

- Stable logical name.
- Description and strict input schema.
- Output schema.
- Timeout and concurrency safety.
- Execution callback.
- Result renderer/finalizer.
- UI render intent.
- Provider-native mapping when applicable.

Dynamic and MCP registration validates the same schema contract as built-ins. A late registration cannot bypass validation.

### Execution pipeline

```text
resolve visible definition
  → freeze arguments
  → pre-execute waterfall
  → approval policy
  → monotonic guards
  → execute waterfall
  → body
  → validate/render output
  → post-execute waterfall
  → finalize
  → durable result
  → contained notification
```

Denial and failure are structured outcomes. A later guard cannot turn deny into allow. Approval is inside the one pipeline so alternate callers cannot bypass it.

### Scheduling

The model may return multiple calls. Scheduling groups calls into:

- Exclusive calls: filesystem/shell mutations or definition-declared unsafe concurrency.
- Parallel calls: read-only/concurrency-safe operations, bounded by a live policy.

Preparation stays model-order deterministic. Dispatch may overlap. Durable call/result events commit in model order so replay is stable. Cancellation prevents new starts and drains started work.

### Deferred tool schemas and Code Mode

Large tool catalogs harm context and cache performance. The tools service supports:

- Native mode: send visible schemas directly.
- Deferred/tool-search mode: send a provider-native or local tool-search capability.
- Code Mode: expose one `run_code` tool whose bindings dispatch to visible tools.
- Both: provider/model chooses based on capability and policy.

Subcalls retain complete audit events while only the curated outer result enters model history. Tool search and Code Mode are plugins, not special loop branches.

## Execution world

### Filesystem

`FileSystem` owns resolution, canonical process paths, read/write/edit, metadata, directories, glob, grep and observation records. Providers include local, sandboxed and future remote/E2B implementations.

Implemented E01/E05 publishes `FileSystemService` through `filesystem-local`; `tools` injects it and read/write/edit/glob/grep are real Consumers. Explicit roots are capability-bound and canonical, observations retain stable identity/freshness, blind/stale writes fail, and mutation rechecks root/parent/target or absence before root-local atomic commit. Traversal, aliases, symlink loops/escapes and deterministic TOCTOU swaps fail. Specs/results/errors remain bounded/redacted; multiline diffs are commit-consistent and search/wildcard work is bounded/iterative.

Required safeguards:

- Read caps and retained-output spill.
- Binary/media detection.
- Read-before-overwrite.
- Freshness or content-hash preconditions for edits.
- Atomic writes where applicable.
- Symlink and path-escape policy.
- Workspace and extra-root enforcement.
- Explicit external-directory approval.

### Subprocess

`Subprocess` owns process-tree creation, environment policy, stdout/stderr streams, signals, PTY mode and quiescent termination. Credential-shaped environment variables are removed unless explicitly granted by reference.

E02 implements the base `subprocess` Service Definition and local Provider: exact absolute launch specs, inheritance-free explicit environment, bounded capture, typed nonzero/timeout outcomes, fixed redacted failures and consuming wait/cancel/terminate/kill handles over private process groups. Text Consumers use interactive lines; framed protocols use pre-decode raw 64 KiB chunks, bounded backpressure and explicit EOF so invalid UTF-8/cumulative output cannot be hidden. E03 routes `bash`; E04 makes sandbox policy mandatory; E10 publishes exact capability choices. POSIX `setsid` weakness remains a visible fact; PTY expansion remains E07.

### Shell

`ShellExecutor` uses `resolve(request) -> spec` as the only defaulting point. The spec includes command, shell, cwd, environment references, timeout, sandbox mode, network request and background behavior.

Non-zero exit and timeout are tool results, while spawn/policy infrastructure failures are tool errors.

### Persistent terminal

`TerminalRegistry` provides owner-scoped PTY sessions and tools:

- `terminal_start`
- `terminal_write`
- `terminal_read`
- `terminal_resize`
- `terminal_kill`
- `terminal_list`

Session ownership, output retention, serialization and process-tree cleanup are enforced by the service.

### Sandbox

The sandbox service separates policy from backend. Backends include Seatbelt, Landlock/seccomp, bubblewrap and Windows native confinement.

Policies describe readable/writable roots, network, environment, devices and process permissions. A backend reports unsupported facts; it never silently downgrades.

All process consumers use the same sandbox service. Filesystem mutation policy and process confinement agree on canonical roots.

Implemented E10 offers only modes with `Supported` backend evidence and reports host networking independently. E12's native Seatbelt path/network/socket matrix passes. E11 fixes exact Landlock ABI rights and adds strict Linux CI; real bwrap/fallback pass, hosted Landlock proof is pending. E13 verifies Windows Job Object tree semantics in cross-build tests but keeps restrictive choices unsupported; native Windows filesystem confinement requires a new spawn-capable Provider seam.

### LSP

The LSP service owns language-server lifecycle and normalized navigation:

- Definition, references, implementation.
- Hover and signature help.
- Symbols and workspace symbols.
- Diagnostics.
- Rename preview where supported.

The model-facing `lsp` tool is a Consumer. Local stdio is the first Provider. LSP processes run through subprocess/sandbox and share workspace paths.

## Web capability

Implemented WEB01 crate/service `heycode-web` / `web` owns logical search and fetch
providers. `heycode-tools` only consumes this service. Providers may be:

- Provider-native server tools.
- OpenRouter server tool.
- Exa/Perplexity/Brave/search API.
- Keyless fallback for development only.
- HTTP fetch with browser-quality extraction.
- MCP provider.

Implemented N03 registers `web_provider:portable`: operation-time Brave when
configured, otherwise DuckDuckGo Lite, plus bounded HTTP(S) fetch. Requests and
provider output are validated/redacted and caller-cancellable. WEB02 validates
every URL authority, denies typed private/mapped IPv4/IPv6 and any mixed/private
DNS answer, pins the approved addresses for the connection and repeats admission
across bounded loop-checked redirects. Rich extraction/citations remain WEB03.

Implemented WEB04 adds Settings namespace `web` and explicit search/fetch
provider ids. When an id is omitted, only one locally available implementation
may auto-select; ambiguity is a visible error. Canonical base-domain allow/block
rules match subdomains with block precedence. Search results are filtered before
tool publication; fetch uses the same immutable policy for initial URL, every
redirect and the validated final URL. Optional plugin `status-web` exposes the
registered provider/capability/availability rows and effective policies at
`/web`. This client seam does not imply that every provider-native server tool
supports the same filters; provider adapters must consume or explicitly refuse
them according to their own evidence.

Search and fetch are separate operations behind one registry. The request resolver may expose provider-native logical tools or heycode tools.

Security requirements:

- DNS and redirect SSRF checks on every hop.
- IP classification after resolution and connection pinning where possible.
- Domain allow/block lists.
- Content type, byte, token and redirect caps.
- HTML/PDF extraction with source metadata.
- Prompt-injection labels and untrusted-content boundaries.
- Durable citations and retrieval timestamp.
- No credentials forwarded across origins.

Implemented WEB03 adds exact `web_processor:portable-readable`. Portable fetch
separates a 4 MiB raw-source ceiling from the readable result ceiling. HTML
renders bounded text and links; PDF uses lopdf's bounded load/per-page APIs,
2 MiB per page, at most 256 pages, page markers, cancellation and a supervised
10-second worker. Successful extraction admits raw bytes through ATT01 and
durably records final URL/title/retrieval/raw-truncation/page metadata. The
model-facing `web_fetch` result cites that same URL/title and never reveals the
content hash. Malformed/encrypted/truncated/bomb-like PDF or unsupported binary
fails before source/event publication. WEB05 adds a typed classification to
both Web tools; successful results commit it in `tool/result`, render a fixed
model-visible data-only warning and show a TUI warning on live/replay. Denials
and fixed local failures do not inherit external content.

## Attachments and multimodality

Implemented ATT01 `AttachmentStore` admits bytes after size, content-sniffed MIME,
safe-name and image-dimension checks. Unix storage is owner-only, immutable and
SHA-256 addressed; reads reverify type/mode/link/identity/length/hash/MIME and
dimensions. The v2 `attachment/added` event commits only after bytes and stores
the address plus metadata, never inline base64. Non-Unix local storage is
explicitly unsupported pending an audited owner-security backend. ATT02 adds
TUI `/attach`, repeatable headless `--image`, immediately-adjacent durable
`user/attachments`, byte re-verification, exact selected-model image capability
proof and Responses/Chat/Anthropic wire projection. Unsupported, Unknown and
legacy routes refuse before user admission. ATT03 adds `/document` and ordered
`--document`, an effect-owned shared extractor, independent document capability
evidence and durable Native/Extracted source→selected routes. Exact Supported
strict routes send native PDF blocks; every other PDF/HTML route commits bounded
derived text. X03 now accepts ACP image/embedded-resource blocks, streams
tool/plan/usage events and cancels through the same runtime/attachment owners.

Inputs:

- Images.
- PDFs/documents when provider supports them or extraction is selected.
- Text/binary files as references.
- Future audio.

The exact model route proves modality support before admission to a request. Switching to an incompatible model offers extraction, removal or cancellation.

## Session and persistence

### Storage

Keep append-only JSONL as the portable source format. Add SQLite indexes for listing/search/telemetry, not as an independent truth.

Persistence providers:

- JSONL.
- SQLite/indexed local store.
- Future remote store.

Session flush is an awaited durability checkpoint used by forks, schedules, goals and shutdown.

### Resume, fork and branch

- Implemented C07 resume/query resolves exact safe ids from bounded JSONL truth.
- Fork creates a new id at a closed/event-count boundary, hashes exact persisted parent-line leaves and stores only lineage plus suffix. Nested/empty/v1 prefixes replay once; tamper/missing/cycle/depth failures are loud.
- Branch-from-message creates a fork at the selected surface boundary.
- Delegated-runtime forks map to native runtime APIs when possible and record external lineage.

### Query and export

Implemented local session query supports deterministic keyset pagination, text, provider/runtime, source, durable status, cwd and direct-parent filters plus latest/resume/fork. Pinned/archived actions and exports remain U15/C10.

Exports:

- Lossless JSONL.
- Human Markdown transcript.
- Redacted support bundle.
- Machine JSON summary with usage/cost/timing.

### Crash recovery

On resume, repair open calls/steps/turns with explicit `not-started`, `outcome-unknown` and `interrupted` facts. Never invent success or replay a mutation automatically.

## Compaction and context

The token meter prices the complete resolved envelope: prompt, tools, messages, provider state, attachments and reserved output. Exact provider token-count endpoints are used when available; deterministic estimates carry an error margin otherwise.

Pressure policy:

1. Model-free prune old oversized tool results.
2. Apply provider-native context editing when selected.
3. Use provider-native compaction when supported.
4. Use portable summary compaction.
5. Recover canonical context-overflow errors only when durable surface progress occurred.

Compaction is a durable transaction and reports strategy, range, usage and cache impact. `/context` explains the current budget by contributor.

## Skills and instructions

Discovery roots support project `.heycode/skills`, `.agents/skills`, compatible `.claude/skills`, user roots and plugin roots with explicit precedence.

Descriptions load into a deferred catalog. Full bodies load on demand. User-only skills cannot be model-invoked. Skill source, version and content hash enter the request header when used.

Workspace instructions support `AGENTS.md` with nested precedence. Importing `CLAUDE.md` is an explicit compatibility plugin, not an implicit conflict.

Implemented CMD05 initializes root guidance through the `init` plugin. It owns only one versioned managed marker range: preview is bounded/read-only, apply requires an exact optimistic token, and all user-authored bytes outside that range are preserved. `/init` is human-only and never enters provider history. Nested instruction loading/precedence remains a separate capability boundary.

## Commands and hooks

Commands run against the receiving session/agent without creating a model turn unless the command owns a logged model input.

Hooks attach to typed lifecycle events:

- SessionStart/End.
- UserPromptSubmit.
- PreStep.
- PreToolUse/PermissionRequest/PostToolUse.
- PreCompact/PostCompact.
- SubagentStart/Stop.
- Stop.
- Config/Plugin/MCP change.

Handler types: command, HTTP, MCP, prompt, subagent. Every hook declares timeout, failure behavior, trust source and model/token effect. Managed policy can restrict hooks.

## Background jobs

`JobRegistry` provides owner-scoped background operations with states running, stopping, completed, killed and failed.

Model-facing tools:

- `job_list`
- `job_output`
- `job_kill`

Producers include shell, terminal, subagent, workflow, test and review jobs. Completion notifications wake an idle agent within a bounded consecutive-wake budget or inject into a busy agent.

## Subagents and delegated agents

`SubagentRegistry` separates provider from tool Consumer.

Providers:

- Fresh native heycode child.
- Forked native child.
- Continuable native child.
- Codex official runtime.
- Claude Code official runtime.
- OpenCode ACP/server.
- Generic ACP or heycode SDK runtime.

The local ACP v1 adapter is implemented for native heycode sessions: bounded
content blocks, rich normalized updates, session cancellation and quiescent
per-session Context ownership. Delegated OpenCode runtime activation remains
R09. X04 adds the distinct stable heycode JSON-RPC v1 service and typed local
client; ordinary foreground TUI turns now use it over the native RuntimeSession.
X05 adds an optional effect-owned control generation for correlated route-bound
authorization, provider/model selection, redacted MCP/plugin inspection and
explicitly wire-exposed Settings CAS. X06 adds low-dependency `heycode-sdk` as the
single Rust wire/client owner and pinned `@heycode/sdk` with independent runtime
validation; shared fixtures and runnable examples cover start, expected-id
resume, typed streaming and concurrent cancellation. External IDE transport and
authentication remain X07/E08 rather than an SDK capability claim.

R04 activates Codex delegated discovery without activating sessions. The exact
0.146.0 runtime reports credential-blind account readiness/plan and paginated
visible models with conservative effort/modality/provider evidence through the
shared `AgentRuntime` methods; it advertises only `models=Supported`. R06 now
owns primary start/resume/fork/steer/compact/events/permissions, while R05 still
owns ephemeral delegated-subagent behavior after O05.

The model-facing tool binds a specific provider or an allowed provider set. Depth, background mode, tools, permissions, runtime, output schema and context inheritance are capability-checked.

Each child has a durable session or external runtime reference. Results return final output and safe structured diagnostics; raw private reasoning and secrets never enter the parent.

## Agent teams

After continuable subagents are stable, add an optional team coordination service:

- Roster and roles.
- Shared task DAG with revisions.
- Peer mailbox.
- Lead/root authority.
- Wait-for-change and bounded wakeups.
- Per-agent workspace/worktree isolation.

Team state is durable and inspectable. This remains experimental until race, recovery and cost behavior are proven.

## Goals

Goals are revisioned full-snapshot events with active, paused, blocked and complete phases. Runtime activation is not silently restored after resume; a human re-arms it.

Goal rounds enqueue explicit source-tagged inputs with round limits and persistence flushes. Tools and `/goal` enforce authority and compare revisions.

## Workflows

Workflows are deterministic scripts run by a worker Provider with explicit host capabilities. Use them for repeatable multi-step automation, not arbitrary hidden model behavior.

Capabilities:

- Versioned workflow definitions.
- Inputs/outputs schema.
- Tool/subagent/job calls.
- Checkpoints and resumability.
- Budget and cancellation.
- Structured progress events.

`workflow` and `ralph` are Consumers. The worker thread/process is a Provider.

## Schedules

Durable session-local schedules support one-shot delay, absolute time and fixed-rate recurrence. They flush before dispatch, append dispatch only after enqueue succeeds and never inherit into forks unless explicitly copied.

Timers clamp platform limits and survive restart through persistence.

## Reviews and quality loops

Review is a plugin contribution that can run on the current model/runtime or a configured reviewer. It supports:

- Working tree.
- Base branch/PR diff.
- Commit.
- Security scan.
- Performance and test review.
- Multi-agent review.

Findings are structured, evidence-linked and optionally fixable after user selection. Review tools do not silently mutate.

## Telemetry and cost

Telemetry records non-secret operational facts:

- Provider/runtime/model.
- Request purpose.
- TTFT, duration and retries.
- Input/output/reasoning/cache tokens.
- Native tool calls and local tool calls.
- Cost when provider supplies pricing/usage.
- Compaction and cache effectiveness.
- Tool latency/error/denial.
- Session/agent/job lineage.

Telemetry is a plugin with local-off default and explicit OTEL/export providers. Session-local `/usage` remains available without remote telemetry.

## Capability acceptance checklist

- [ ] The native loop supports follow-up, steer and inject with durable queue accounting.
- [ ] Cancellation is reusable across later turns and closes durable anchors.
- [ ] Parallel tools dispatch concurrently but commit in model order.
- [ ] Every mutating operation crosses the approval/sandbox policy once.
- [ ] fs/subprocess/shell/terminal/LSP providers can be swapped without provider-specific tool forks.
- [x] Web redirects cannot escape SSRF policy.
- [x] Image bytes are content-addressed, owner-secured, durably associated and projected only after exact model/protocol capability proof.
- [x] PDF/HTML routing is durable and explicit: exact native capability sends protocol file blocks; all other routes use one bounded composed extractor.
- [ ] Request reconstruction includes prompt, tools and provider state.
- [ ] Native and portable compaction converge under stress.
- [ ] Hooks, jobs, subagents, goals, workflows and schedules dispose to quiescence.
- [ ] Delegated runtimes preserve their native auth and never expose tokens.
- [ ] Session export/replay and crash repair are deterministic.
