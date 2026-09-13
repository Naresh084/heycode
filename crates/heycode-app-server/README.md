# heycode-app-server

X04/X05 stable local JSON-RPC protocol, effect-owned service, and optional
registry-backed control contribution.

Plugin `app-server` injects the composed Agent and runtime registry and provides
service `app-server`. Protocol version 1 owns `initialize`, `session/open`,
`session/configure`, `runtime/models`, `turn/start`, `turn/cancel`, and
`session/close`, plus monotonically sequenced `session/event` notifications.
Public wire types and the transport-neutral client live in low-dependency
`heycode-sdk`; this host consumes/re-exports them and implements `AppTransport`
directly for `AppServer`. `LocalAppClient` is that generic SDK client, so every
local request, response and event traverses raw JSON exactly like a future
stdio/socket transport without spawning a bridge.

`session/open` accepts the exact runtime configuration controls (system prompt,
ordered tools, model and reasoning effort) and returns both their effective
values and per-field capability evidence. `session/configure` requires the
matching session to have been explicitly opened, validates that identity before
dispatch, applies supported between-turn updates, and returns the effective
merged configuration. Delegated host-tool callbacks pass through the Agent's
approval and durable session log before the provider receives a result; event
projection deduplicates the callback and adapter views only within the active
turn. In ask mode, the server owns an `InteractiveApproval` typed subscription
only while its turn is active, projects each pending Agent decision as a
randomly namespaced `agent-request-*` permission event, and routes the client's
allow-once, allow-session or deny answer back to the exact waiter. Argument-free
tools still carry an explicit `(no arguments)` detail. Adapters whose provider
protocol permits request-id reuse mint distinct
session-wide runtime call ids before crossing this boundary.

`ask_user_question` is a shared Agent tool, so every native provider that can
call host tools receives the same schema and delegated runtimes receive it when
their runtime supports configured host tools. One cancellation-aware broker
serializes concurrent questions and binds each waiter to one active surface.
The event carries an optional short header, ordered labels and aligned optional
descriptions; clients may return a selected label or custom text, or explicitly
cancel. A headless/auto surface never fabricates an answer: without an active
subscriber the tool fails immediately. Runtime-native question callbacks use
the same event and response wire when the adapter supports them.

Optional default plugin `app-server-controls` injects the existing settings,
credentials, authorization, secret-prompt, provider/model, MCP, routing and
plugin-inventory services. It contributes exact version-1 methods for:

- authorization catalog/start/answer/cancel/logout;
- provider/model list and durable selection;
- runtime discovery/durable selection and canonical allowed-root workspace
  selection;
- redacted MCP and exact plugin inventory snapshots; and
- settings list/get/CAS replacement.

The contribution registers and disposes atomically without changing the base
`app-server` service, so deliberately minimal profiles retain turn hosting.
Authorization is bound to the effective provider's owned credential reference,
uses an opaque correlation id, and publishes only masked prompt metadata plus a
secret-free post-commit receipt. Catalog listing never probes a keychain; its
credential state is explicitly uninspected until commit/readback. Dropping a
prompt future revokes its pending answer authority.

Settings values cross this wire only when the namespace schema explicitly calls
`with_wire_exposure`; all other namespaces remain visible by id/revision but
their schema and values are absent. Replacement uses the settings service's
expected-revision CAS, so persistence and owner watchers finish before the
response. Model fallback shows both the current and provider-default ids while
only the provider default is selectable without catalog proof. MCP projection
reuses the registry's redacted snapshot and plugin projection reuses the live
exact inventory.

The production backend opens the current native RuntimeSession, carries durable
attachment metadata, maps normalized runtime events, and reads the committed
user attachment route from the session log. It spawns no work: the calling UI
task owns and awaits each turn. Cancellation is one caller token plus the
runtime session's cancel method; Context shutdown cancels the service before the
Agent/runtime effects unwind.

Each runtime session is subscribed to exactly once and every turn pumps that one
stream. A subscription is contiguous from sequence zero for its whole life, and
a session that outlives the runtime's retained event window renumbers each new
subscription from zero, so a sequence baseline carried from an earlier
subscription would sit above the whole replayed window and silently drop every
event of a later turn until that turn timed out. There is therefore no
cross-turn sequence dedupe at all, and a second concurrent turn on one session
is refused with `Conflict` instead of splitting that stream across two pumps.
When a turn's stream or projection fails, the backend cancels the runtime's
work, returns the failure, and forgets the subscription; the next turn
resubscribes and skips the replayed history up to the first `TurnStarted` it
has not projected before, so one broken turn neither poisons the session nor
duplicates its transcript. Every `AppServerError` mapped from a runtime failure
keeps the runtime's own redacted one-line message as its `detail`, shown after
the fixed message and carried on the JSON-RPC wire as `error.data.detail`.

Provider and model changes still delegate to `RoutingService`, which commits
Settings CAS before updating the live Agent. Runtime selection additionally
rebinds the app-server backend only after that commit and is refused once a
runtime session opened. A read gate held for the whole turn makes every route
change fail with conflict while work is active, so it affects a later turn
rather than an in-flight request. `workspace/select` rejects relative,
dot-segment, missing, non-directory, symlink-escaped and outside-root paths;
the native runtime remains fixed to the composed workspace, while a delegated
runtime receives the canonical selected path on its next start or resume.

C14 adds one more route conflict: a provider/model selection that crosses the
winning native compaction checkpoint requires an explicit portable/fork/cancel
choice. App-server v1 has no field for that choice, so controls return stable
`Conflict` before Settings mutation; they never silently discard or translate
opaque state. The human `/provider` command owns the three resolution paths.

ATT04 adds closed metadata-only `assistant_audio`. The native runtime remains
text-only: immediately before forwarding turn settlement, the backend reads
only the current session suffix and emits new durable `assistant/audio`
associations. Encoded samples are neither a runtime Notice nor a JSON field.
The hidden route adds no initialize flag, factory, method or provider claim.

Delegated runtime usage events may also carry additive `context` metadata with
the exact latest-request token count and provider-reported context window. The
host forwards that evidence independently of the turn-aggregate usage retained
for settlement; absent measurements remain absent on the wire.

X06 also ships pinned `sdks/typescript` and one shared Rust/TypeScript v1
fixture. Both runnable examples cover host-selected start, expected-id resume,
typed event streaming and concurrently awaited cancel.

X07 adds `serve_stdio_transport` as an explicit local child-process bridge over
the same `AppServer`. An outer bounded NDJSON envelope carries one safe-integer
operation id around unchanged app-server request, notification and response
objects, allowing a long turn to coexist with exact permission/cancel calls.
Duplicate/mismatched correlations fail, malformed input cancels and joins every
admitted operation, EOF settles the single writer, and no raw frame is logged.
The extension directly spawns one configured executable with no shell, so this
path creates no socket, endpoint file or cross-user listener. The composition
root exposes it as `heycode app-server --stdio-v1 --workspace <absolute-path>
[--resume <session-id>]`, gives stdout exclusively to protocol frames and shuts
the Context down after EOF, Ctrl+C or transport settlement. The host defaults
to restricted workspace authority unless an explicit trust flag precedes the
subcommand. Rust and Node tests share
`sdks/fixtures/app-server-stdio-v1.json` for both envelope directions.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-app-server --all-targets -- -D warnings
cargo test -p heycode-app-server
```

## Connection setup update — 2026-09-05

Delegated session open forwards the persisted runtime model separately from the native provider/model tuple. The TUI defers opening this backend while onboarding or workspace trust is pending.

The product is terminal-focused. AppServer supports the local `--stdio-v1`
transport; the HTTP listener, browser dashboard, remote host commands and
webhook endpoints were removed at the user's request on 2026-09-08. Native
terminal schedules, inbox delivery, workflows and Plan review remain available.
