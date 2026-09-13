# heycode-runtime-codex

R03/R04/R06 process/wire, credential-blind discovery and primary-session
Provider for the official Codex app-server. It is deliberately separate from
inference Providers: Codex owns its model/tool loop while heycode owns executable
policy, connection lifecycle, event normalization and durable projection.

## Pinned protocol baseline

- Codex CLI: exact `0.153.2` (`codex-cli 0.153.2`).
- Transport: `codex app-server --stdio --strict-config`.
- Wire: newline-delimited JSON-RPC-shaped objects with no `jsonrpc` member.
- Handshake: one correlated `initialize`, validate its result and runtime
  version, then emit `initialized`.
- Stable API only; R03 does not opt into `experimentalApi`.
- Official reference: <https://learn.chatgpt.com/docs/app-server>.

The upstream command is currently labeled experimental. Exact pinning and
fail-loud schema review are compatibility controls, not a claim that upstream
has declared the interface production-stable.

Generated locally with `codex app-server generate-json-schema --out <tmp>`:

- `codex_app_server_protocol.schemas.json` SHA-256:
  `e8284c5cb8157554a3dd1e035aadbd4325aea501af56887e9c2e12eb1b9b9448`
- `JSONRPCRequest.json` SHA-256:
  `31bd6f360b2dd8a7ceaf682708105d40d38cb0b9d0821357a04da67028438f73`
- `v1/InitializeResponse.json` SHA-256:
  `62ad689c2cb6379913c1d72749cfd8de5089d35760214123518eb92eef11acc9`
- `v2/GetAccountResponse.json` SHA-256:
  `08a7dd8c570c905b0bb6998d43ed133e72e2445f08125f86dfc96887e288701a`
- `v2/ModelListResponse.json` SHA-256:
  `c7b58b332f6cf18fd64235409a6daf27bb9e6c09d12dcd0daa2f3dc628b55f6f`
- `v2/ModelProviderCapabilitiesReadResponse.json` SHA-256:
  `5c42590f728289daaa441b947617398a0ca3aa17479013b3c564a5d314a3c9cf`

The bound executable's `--version` and the initialized server's `userAgent`
must both prove the exact release. Any other release fails before higher-level
methods run; upgrading requires regenerating/reviewing the schema and fixtures.
Successful version probes may emit nonfatal startup diagnostics on stderr, such
as a sandbox denial when creating PATH aliases. Diagnostics are discarded;
nonzero exits, truncated output, malformed stdout and version mismatches still
fail closed.

The host's process sandbox also applies to the app-server control process. The
official CLI needs writable state beneath its own home; an ordinary installation
whose home lies outside the permitted workspace can therefore fail initialization.
The runtime reports a fixed actionable diagnostic for failed initialization under
a known restrictive host sandbox. It never copies credentials, changes the sandbox,
or rejects a writable custom home before attempting initialization.

## Boundary and lifecycle

- The common `subprocess` service resolves and launches every process; this
  crate contains no direct process spawn.
- Child environment inheritance is cleared. The composition root must supply
  a reviewed complete allowlist. Every configured `PATH` component must be a
  canonical absolute directory outside the workspace; empty, relative and
  workspace-controlled components fail, while absent absolute entries are
  dropped and at least one usable directory remains required.
- A Homebrew-style `/usr/bin/env node` shim remains supported. The runtime
  resolves `node` from that sanitized `PATH`, requires a native interpreter,
  and launches the absolute interpreter with the canonical script path. It
  never delegates that lookup back to ambient `env` behavior.
- The canonical executable/script and any resolved interpreter are SHA-256
  bound at resolution and rechecked immediately before version and app-server
  launch. This detects replacement inside the operator-controlled installed
  binary boundary; it is an integrity/compatibility check, not publisher
  authentication or a signature.
- Version stdout is capped at 4 KiB. App-server stdout is consumed as exact
  raw chunks with no cumulative connection cap. Each JSONL frame is bounded at
  1 MiB, must be valid UTF-8, and has independent JSON depth/node bounds.
- Retained responses and inbound events share an 8 MiB weighted byte budget;
  permits release when their payloads are consumed or dropped. The raw channel
  and event channel are independently bounded, and overload is terminal with a
  fixed body-free error.
- Bodies, request ids, paths, argv, environment values, stdout and stderr do
  not enter ordinary `Debug` or errors.
- Requests correlate out of order. Canceled post-write requests retain a
  bounded abandoned-id record so one late response is discarded safely.
- Caller cancellation is connected to version execution, server spawn,
  handshake, reads and writes. Cancellation during a partial write makes the
  connection terminal and waits for the contained process group to settle.
- Request admission and terminal transition share one lock, while one sticky
  lifecycle token wakes request and event waiters across every lock/wait race.
- Close is idempotent and settles reader, stdin and the reported containment
  group in that order. `CodexAppServerClient::containment()` exposes the exact
  host facts. In particular, POSIX process-group containment reports
  `resists_session_escape = false`; deliberately detached sessions are not
  claimed as part of the close guarantee.

## Account and model discovery

- `account/read` always sends `{refreshToken:false}`. The typed parser accepts
  only pinned API-key, ChatGPT and Bedrock account variants. ChatGPT email is
  bounded/validated then discarded; the runtime label contains only the closed
  plan id. No token/auth-file/value reaches heycode or Debug.
- Null account plus `requiresOpenaiAuth:true` is Disconnected; false is the
  distinct NotRequired state. API-key/ChatGPT/Bedrock objects are Connected with
  static source or plan labels.
- `modelProvider/capabilities/read` precedes paginated `model/list` with
  `includeHidden:false`, limit 100, at most 64 pages/4,096 rows, non-repeating
  cursors and globally unique ids/aliases. Each operation owns a fresh pinned
  connection and closes it on success, cancellation or parse failure.
- Effort rows/default, input modalities, visibility/default/upgrade fields and
  provider booleans are structurally validated. Generic model evidence remains
  conservative: image/reasoning are exact per model; web-search true/false is
  exact; namespace-tools true proves tool support while false remains Unknown;
  image-generation is not confused with image input; limits/lifecycle stay
  Unknown except an explicit upgrade becomes Deprecated with its replacement.

## Primary sessions (R06)

- `start` / `resume` / `fork` map to `thread/start`, `thread/resume` and
  `thread/fork`. The returned pinned thread id is validated, and each session
  owns its connection, its driver task and one lifecycle child token for the
  whole session — not per operation.
- Start forwards an exact system prompt as `baseInstructions`, provider model,
  reasoning effort and ordered host tools as `dynamicTools`. Explicitly empty
  tools remain `dynamicTools: []`; any supplied tool catalog enables the pinned
  experimental handshake because that method is experimental. Resume/fork
  reapply prompt, model and effort but reject tool-catalog changes; live
  configuration rejects prompt/tool changes, while `thread/settings/update`
  changes only model and effort between turns.
- `item/tool/call` is accepted only for a name in the exact configured catalog.
  It publishes correlated tool call/result events around the host executor and
  replies only after the executor's durable result. Each active turn owns the
  executor cancellation token, so interrupt and close retract a blocked call.
- The session drives `turn/start`, `turn/steer` (with the required
  `expectedTurnId` precondition), `turn/interrupt` and `thread/compact/start`.
  One async operation gate serializes send and compact, so a turn cannot begin
  while compaction is in flight and vice versa.
- Notifications normalize into R02 sequenced runtime events: assistant text,
  reasoning, tool call/output, plan updates, usage per inner model step and
  exact turn settlement. Unknown or uncorrelated frames fail loud; no provider
  body or raw frame reaches the event stream, `Debug` or errors.
- Events publish through the shared `heycode_runtime::RuntimeEventHub`, which
  validates every emission at its call site and bounds retention by eviction
  rather than by refusing to emit. A turn longer than the retention window
  settles normally and later subscribers still get a valid replay from
  session-ready.
- A thread that completes with no agent message is still a stopped turn: the
  adapter publishes the empty final message R02 requires instead of failing the
  session.
- Server→client requests correlate through a pending table. Exactly four pinned
  requests are answered: `item/commandExecution/requestApproval`,
  `item/fileChange/requestApproval` and `item/permissions/requestApproval`
  become `RuntimePermissionRequested`, and `item/tool/requestUserInput` becomes
  `RuntimeQuestionRequested`. `respond_permission` and `respond_question` answer
  the exact pinned request id and refuse a mismatched kind; answering an unknown
  id is a conflict, never a silent accept. Every other server request — legacy
  `applyPatchApproval`/`execCommandApproval`, `mcpServer/elicitation/request`,
  `attestation/generate` and
  `account/chatgptAuthTokens/refresh` — fails the session loudly rather than
  being answered. heycode never services a token refresh.
- Inbound notifications are a closed pinned union. Fourteen methods dispatch
  into runtime events; the remaining sixty-seven pinned methods are explicitly
  ignored because they carry no state this session owns. Only a method outside
  the pinned union is a protocol error. This distinction is load-bearing: Codex
  auto-names threads (`thread/name/updated`) and reports MCP startup and skill
  changes during ordinary turns, and treating those as drift would kill a
  healthy session. An audit test proves handled ∪ ignored equals the pinned
  `ServerNotification` union exactly, and that each list matches the dispatcher.
- `close` and caller cancellation cancel the lifecycle token, join the driver
  task and settle the contained process group before returning; the session
  never leaves an orphan thread or process tree.
- `AgentRuntimeRegistry` advertises `models`, `resume`, `fork`, `steer`,
  `permissions`, `questions` and `compaction` as Supported for `codex`.

## Ephemeral delegated subagents (R05)

The Agent-owned generic runtime adapter starts Codex with `ephemeral:true`,
creates a distinct durable heycode child session, consumes the normalized event
stream, forwards safe permission decisions and closes the app-server session
before returning one final result. A tool-free installed subscription canary
previously passed against release 0.153.2; the 0.153.2 turn canary remains pending without opening auth files or reading
token values. Deterministic adapter fixtures separately prove permission,
plan/tool, cancellation and durable-final behavior.

## Intentional non-goals

- `follow_up` stays Unsupported. Queued delivery is not implemented by this adapter; `thread/inject_items` appends raw Responses items
  directly into model-visible history and is a different contract.
- Attachments in `RuntimeInput` are refused: the pinned `UserInput` mapping
  implemented here is text-only.
- Registration still performs no eager discovery and launches no process.

## Verification

```sh
cargo fmt -p heycode-runtime-codex -- --check
cargo test -p heycode-runtime-codex
cargo clippy -p heycode-runtime-codex --all-targets -- -D warnings
HEYCODE_CODEX_E2E=1 cargo test -p heycode-runtime-codex --test app_server \
  'unix::configured_local_codex_0153_handshake_is_gated_and_credential_blind' -- --exact
HEYCODE_CODEX_ACCOUNT_E2E=1 cargo test -p heycode-runtime-codex --test app_server \
  'unix::configured_local_codex_0153_account_and_models_are_gated_and_credential_blind' -- --exact
HEYCODE_CODEX_SESSION_E2E=1 cargo test -p heycode-runtime-codex --test app_server \
  'unix::configured_local_codex_primary_turn_and_tool_are_explicitly_gated' -- --exact
HEYCODE_E2E=1 cargo test -p heycode-agent \
  live_installed_codex_ephemeral_subagent_is_explicitly_gated -- --exact
```

The handshake gate uses a temporary `CODEX_HOME`. The separate R04 gate uses
the installed runtime's normal official account store but calls only
credential-blind `account/read` (`refreshToken:false`), provider capabilities
and model metadata; heycode never opens auth files, resolves token values, starts a
thread or runs a turn.

## Connection setup update — 2026-09-05

The reviewed installed pin is 0.153.2, including its account-plan variants and closed notification union. Plugin-owned discovery uses an effect-owned temporary workspace; thread start retains the caller’s working directory. Explicitly enabled account/model discovery and a tool-free subscription turn passed on 2026-09-05.

Runtime descriptors may carry bounded provider-owned connection recovery instructions. The subscription wizard displays them after account or installation checks fail and keeps Enter available for retry; sign-in is performed through the official app.
