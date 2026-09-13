# heycode-mcp

Model Context Protocol registry, the stdio JSON-RPC bridge and the Streamable
HTTP transport. MCP is not a special CLI branch: the `mcp-registry` plugin
publishes service `mcp`, and transport plugins publish definitions/connections
and effect-owned tool registrations through the ordinary plugin context.

## Registry contract

- Definitions are exact, private and generation-tracked. Redacted inspection
  exposes source kind/reference without env values, auth material or process
  details.
- Validation rejects ambiguous names, unsafe commands/URLs, secret-prone
  public metadata and definition/auth mismatches before publication.
- A successful generation swaps atomically. Failed refresh can retain an
  explicitly visible last-good generation; stale handles cannot clobber a
  replacement.
- Shutdown is terminal and effect-owned.

## Transport-neutral channel

Stdio and Streamable HTTP differ in framing, not in the JSON-RPC contract, so
both publish one `McpRequestChannel` (`call(method, params, cancellation)`).
`McpChannelError` is the shared closed failure set — transport, timeout,
cancellation, JSON-RPC code, static protocol requirement, unauthorized,
conflict — and maps onto the registry's `McpFailureCode`. No server body, URL,
header or credential byte enters it. `McpServerHandshake` validates one
`initialize` result once, for every transport.

## Atomic paginated tool generations

`McpToolGenerationOwner` is the single lifecycle owner of one server's tool
rows.

- The `tools/list` walk is bounded by `McpToolListLimits` (pages, rows per page,
  total rows) and rejects a repeated cursor. **A missing or `null` `nextCursor`
  ends the walk; an empty string is a valid cursor and means more results
  follow** — never test the cursor for emptiness.
- Every row is validated before it can become model-visible: name segment,
  bounded description, object-only `inputSchema`. One bad row rejects the whole
  generation.
- The walk runs while the previous rows stay live. A `McpListChangeWatch` epoch
  captured before the walk must still hold at the end; a
  `notifications/tools/list_changed` observed by either transport during the
  walk marks the walk torn and it is discarded unpublished. This is a security
  control, not only a correctness one: a replayed or injected list change must
  not be able to swap a client's tool set behind a torn read.
- Only a complete, unraced candidate swaps the rows. Registration is
  token-owned and all-or-nothing; a contested name rolls the candidate back and
  **restores the previous rows**, and only if that restore is impossible does
  the registry stop claiming a generation. The registry generation number
  advances exactly once, at publication.
- Concurrent refreshes serialize on one swap lane, so the live rows are always
  exactly one complete walk.
- Dropping the owner removes every row, even from a separately held
  `ToolRegistry`.

## Elicitation, progress and logging

MCP11's lower product boundary is `McpClientEventRouter`. One router binds one
validated `McpServerId` to one opaque, debug-redacted `McpClientRoute`; neither
the focused window nor a server-supplied string chooses the destination.

- `elicitation/create` admits only declared 2025-11-25 form/URL modes. Form
  schemas are bounded flat primitive schemas and accepted content is validated
  again before reply. URL mode requires a credential-free HTTPS URL and keeps
  its opaque completion id as an owned row.
- Duplicate server request ids never replace a pending row. Exact cancellation,
  completion, pending-handle drop and transport shutdown retire only their own
  interaction; cancellation sends no late response.
- Client-generated progress tokens exist only while an operation registration
  is live. Unknown, late, malformed and non-increasing updates are ignored.
- Structured logs are size/level/logger validated and remain on the routed
  human event sink. Arbitrary JSON has no model/session projection, while
  Debug reports lengths rather than bodies or route values.
- Stdio advertises the configured elicitation capability, responds without
  blocking its read loop, and retires an outbound pending row before sending
  `notifications/cancelled`. The stdio driver owns those handler futures in a
  local `FuturesUnordered`, cancels and drains them before its runtime can
  retire, and never discards a spawned task handle. Router shutdown also makes
  late logging/progress/completion frames inert — but a crashed stdio child is
  *not* a shutdown: the driver loop calls `retire_pending()`, which cancels the
  rows the dead transport owned and leaves the route usable, because the
  reconnect supervisor hands the replacement child this very router and the
  product UI holds it for the whole session. Terminal `shutdown()` belongs to
  connection disposal alone; latching it on a crash would answer every later
  elicitation `-32603` and drop every later log and progress frame in silence. Streamable HTTP now acquires the response head
  through `HttpService::stream_response`, dynamically selects JSON or SSE, and
  owns body parsing, pending handlers and concurrent reply POSTs in one local
  `FuturesUnordered` loop—no detached task. Open streams may wait for an
  elicitation reply before emitting their final response; cancellation still
  retires the exact interaction and emits no late reply.

`McpProductSession` in `heycode-tui` now owns the product route, elicitation broker
and progress/log sink. It mints at most one router per server/session;
`mcp_product_plugin` requires the configured server and router sets to match,
and `mcp_bound_servers_product_plugin` refuses any enabled bound connection
without a router. `spawn_with_client_events` remains available for non-product
protocol embedding. The default root now
selects the product constructor with one pre-minted durable session route per
configured server.

## Per-server/tool policy

MCP13 is enforced by policy-aware generation/plugin paths. Every `tools/list`
row parses bounded advisory annotations before the candidate can publish.

- Exact allowlists hide outside names; disabled or `Deny` rows also stay out of
  the model-visible generation. The committed count is the filtered count.
- `Allow` skips only the MCP-specific prompt, never ordinary higher host guards.
- `Prompt` calls `McpToolApprovalHandler` with exact server/tool/arguments
  before `tools/call`. Server annotations are structurally absent, so a claimed
  `readOnlyHint`, `idempotentHint` or vendor extension cannot weaken policy.
- Denial/cancellation sends no server request. A policy-aware call reserves an
  MCP11 progress token and retires it when the call settles.

`mcp_plugin` remains the PL04 bridge for a trusted plugin host. It has no
approval broker, so a `Prompt` admission there proceeds to the host's own tool
gate rather than failing — but its allow/deny filtering is identical, because
both constructors carry the definition's policy.
`McpProductSession::approval_handler` now adapts the exact ordinary Agent
`ApprovalPolicy` at invocation time. Its request contains only the qualified
tool and arguments—server annotations remain structurally absent. Root uses
this product constructor, so MCP13 is reachable in the default world.

## Credential-aware bound connections

`McpBoundServer` joins an exact private definition to a complete
`McpCredentialBindings` set. Bindings carry only `CredentialQuery` plus
`Raw|Bearer`; they cannot rename an erased reference or retain a value. Missing,
extra, mismatched and Bearer-for-stdio bindings fail before activation.

`mcp_bound_servers_plugin` injects the ordinary MCP registry, tools,
subprocess, HTTP and credential services. Stdio resolves at process launch;
HTTP resolves independently for each request so rotation reaches the next
operation. Both reuse the existing handshake, sibling walks, atomic
policy-aware generation, rich-result parser and effect-owned teardown.

`McpBoundServer::provider_stdio` and
`provider_streamable_http_bearer` are the shared PMM04/PZA05 root-factory
builders. They accept canonical provider launch facts, exact tool allowlists
and `CredentialQuery`/reference pairs only; they force Prompt approval, disable
resources/prompts/instructions and never accept a credential value.

## MCP15 local conformance matrix

The checked-in fixtures under `tests/fixtures/` are credential-free local
servers, not mocks returning values in-process:

- `mcp15_server.py` speaks real newline-framed stdio and real loopback
  Streamable HTTP. It exposes tools/resources/prompts/logging, an ordered rich
  result with `structuredContent`, hostile JSON-RPC errors, slow cancellation,
  and both finite and reply-gated open SSE elicitation/progress/logging frames.
- `mcp15_oauth_server.py` is a stateful authorization and resource server. It
  records the authorization request's exact resource/state/S256 challenge,
  validates the code verifier at token exchange, issues and refreshes only
  audience-bound fixture tokens, and returns canary-bearing hostile bodies that
  must not cross diagnostics.

`tests/it/mcp15.rs` drives the public production `McpConnection` stdio path and
`McpStreamableHttpClient` over `ReqwestHttpTransport`. OAuth discovery,
authorization, callback checks, exchange, refresh and protected-resource use
cross a real loopback socket server through the public `HttpTransport` boundary
while retaining semantic HTTPS issuer/resource URLs. That last adapter omits
TLS only because the production reqwest client intentionally has no test-CA
injection seam; it does not weaken production URL validation.

On 2026-08-31 the official current Inspector package was
`@modelcontextprotocol/inspector@2.4.0`. It was invoked with `npx` from a fresh
temporary npm cache and temporary catalog/client-config paths. Its `--cli
--strict` client successfully listed the fixture schemas and called the ordered
rich-result tool over both stdio and Streamable HTTP; it also initialized the
protected resource with a temporary fixture bearer token. This is genuine
local Inspector interoperability, not hosted-server or real-credential
evidence.

MCP11's protocol-side open-stream gap is closed. The local fixture holds its
original chunked SSE response open until heycode concurrently POSTs the accepted
elicitation response, then emits progress/logging and the matching result. A
second open stream cancels the pending elicitation and proves no response POST
occurs. Default central composition now mounts the concrete Agent/TUI
attachments over the same route and lifecycle owner.

O09 is attached at the generated tool's actual call boundary. Pre runs before
MCP-specific approval and `tools/call`; a deliberate refusal sends nothing.
Post runs only after a complete result parses. Both are awaited under the tool
cancellation token, carry no contribution text back through the port and leave
no spawned task. The TUI product adapter durably commits any successful
contribution before the next Agent request can project it.

## Rich tool results

`McpToolResult` is the MCP12 protocol boundary. It retains text, image, audio,
resource-link and embedded-resource blocks in exact server order, with each
block's validated audience/priority/modification annotations and bounded
unknown extension members still attached to that block. Unknown block kinds
are refused rather than skipped because dropping one would change the result.

Resource URIs must be bounded absolute RFC 3986 URIs. HTTP(S) resource links
also pass through core's credential-free public-source validator; file, git and
custom schemes remain resource URIs without being mislabelled as public web
sources. Decoded media carries an ATT01-compatible canonical media type and the
SHA-256 content id that durable admission will derive, while `Debug` exposes
only kinds, counts and lengths.

`structuredContent` preserves every JSON value, including JSON `null`, and the
tool generation retains the declared `outputSchema`. The deliberately partial
schema evaluator reports `NotChecked` for every assertion or dialect it cannot
evaluate; it never reports `Conforms` after ignoring an unknown assertion.
Absent and explicit `null` are distinct at protocol fields whose schemas make
that distinction.

The registered `Tool` path returns a typed pending rich plane in addition to a
body-free JSON UI projection. Agent commits media/blob bytes through ATT01 in
the scheduler's model-order cursor, then appends v2-only `tool/rich-result` with
durable attachment references. A crash between those commits leaves an honest
open tool call plus admitted bytes, never a phantom result. Provider text is
reconstructed from the durable ordered blocks; TUI/runtime receive typed
metadata and `UntrustedContentSource::Mcp`. Server-reported `isError` results
retain the same rich blocks and error bit rather than collapsing to one string.

## Non-secret competitor import

`preview_competitor_mcp_import` is MCP14's crate-local bridge over S13. It
accepts only `heycode-config::CompetitorImportPreview`; it never receives source
bytes, file paths, credential values, headers or environment maps. Provider,
settings and unknown-field values are not copied into the MCP preview.

Clean, authority-ready MCP rows become private exact `McpServerDefinition`
candidates with reconnect disabled. Project scope and executable readiness are
preserved exactly from S13—there is no second boolean that can widen them—and
whole-set definition extraction refuses trust gaps, executable gaps, name
collisions and enabled incomplete rows before returning any candidate.

Credential/environment/header/OAuth exclusions become deterministic
`McpSecretReference` requests carrying only source field path and binding role.
Because S13 intentionally erased header/environment names and every value,
MCP14 does not invent a wire binding. Disabled incomplete rows remain visible
with their unresolved references but produce no definition; enabled rows fail
until a future management/UI owner explicitly binds those references through
the credential registry.

Current source facts remain grounded in the official
[Codex configuration reference](https://developers.openai.com/codex/config-reference/),
[Claude Code MCP guide](https://code.claude.com/docs/en/mcp), and
[OpenCode MCP guide](https://opencode.ai/docs/mcp-servers/).

## Live health for `heycode mcp test|auth|list`

Listing never connects: `McpManagement::list` is a store read and reports
`Unknown`, because opening a panel or completing a name must not start every
configured server. `list_probed` is the version that asks, and `McpProbe`'s
provided `probe_all` runs up to eight probes at once, so `heycode mcp list` and
the panel's "Refresh and probe" cost about one probe rather than N.
Composition connects the same way: every configured server's handshake is
driven together on one plain thread (`drive_all_on_plain_thread`), so three
slow servers no longer hold the shell blank for the sum of their startup
budgets. Disposal is registered for every server that did start before a
required failure is reported, so a rollback still kills what this apply
created.

`management::LiveMcpProbe` is the production `McpProbe`: a stdio definition
is spawned exactly as a session would spawn it and dropped again (which kills
the child); an HTTP definition gets one `initialize`, where 401/403 means
`AuthorizationRequired`. Both are bounded (10 s by default), so a server that
never answers is `Unreachable` in bounded time. The probe owns a thread and
a runtime of its own — the trait is synchronous and may be called from inside
another runtime — and drops the connection's driver runtime off the async
context. The CLI's management world installs it via
`McpManagement::detached_with_probe`; the interactive world keeps its real
registry evidence and installs none. `McpConnection::spawn`'s failure path
now releases its runtime on a plain thread too, so a missing binary is an
error rather than a Tokio panic.

## One server surface

The management store (`mcp-servers` settings namespace, written by
`heycode mcp add/edit/enable/remove`) and the `[mcp.servers]` configuration
section are two sources feeding **one** set of connections, joined inside
`McpPlugin::apply` in product mode:

- Every enabled row the store holds becomes a connection the session actually
  makes. A configured name wins a collision, because its transport is exact
  where a stored row is a bare command or URL.
- **No server is required unless it says so.** Every server is user data — a
  `[mcp.servers]` entry as much as a stored row — so a typo, a not-yet-installed
  binary or a server that is down publishes its classified failure, stays
  visible in `/mcp` as `Failed`, and composition continues to the next server.
  Only `[mcp.servers.<name>] required = true` opts a server in: then a failure
  to connect fails composition with a message that names the server and its
  program. A stored row is never required.
- `heycode mcp add` validates what a session would validate. An HTTP target must
  be an acceptable Streamable HTTP URL (no userinfo, query or fragment) at the
  moment it is typed, with the same message the connection path would give. A
  stdio row may carry arguments after `--` and environment via
  `-e KEY=VALUE`; `-e KEY` alone stores an empty value meaning "inherit the
  host's variable at launch", and a value that looks like a credential is
  refused so secrets never enter the store. Unknown flags are refused, never
  dropped.
- The startup handshake is bounded at 10 s by default (`startup_ms`), tighter
  than the 30 s request timeout, because every server is connected before the
  shell appears and one server that never answers `initialize` must not hold
  the product blank for half a minute. Connecting asynchronously after
  composition remains open work.
- Releasing a connection at exit hands the driver runtime's last reference to
  a plain thread: effects unwind inside the host runtime, where a blocking
  runtime shutdown panics.
- Every configured server is adopted into `McpManagement` for the life of the
  context, so `mcp list` and the `/mcp` panel see the servers the session is
  really running. Those rows refuse every mutating operation with
  `McpManagementError::ConfigDeclared`, which names the file to edit instead:
  the settings store is not where they live.

`McpHostMode` decides which half applies. `mcp_plugin` (the PL04 bridge) has
neither the HTTP service nor the management store behind it and injects
neither; `mcp_product_plugin` injects both, so a missing one is a composition
error rather than a silently reduced feature set.

`heycode mcp` composes a management-only world with no configuration behind it, so
it still sees the store alone. Giving that surface the configured rows is a
composition-root change, not a change here.

## Plugin-bundled stdio servers

PL04 packages freeze their MCP JSON document with the immutable PL02 object.
The product host resolves `command` and optional `cwd` strictly relative to
that verified root, rejects traversal, symlink escapes, non-files and (on Unix)
non-executable targets, then supplies the ordinary MCP plugin with a complete
definition. Metadata, timeouts, reconnect and tool
allow/deny/approval remain definition policy — every generation owner is built
from an exact definition, so a bundle's allowlist and deny rows filter its
published generation exactly as a user-configured server's do, and the bundle
does not bypass MCP13 approval enforcement. `McpExposurePolicy.resources` is
enforced by the model-facing `list_mcp_resources` and `read_mcp_resource`
tools; package definitions that do not opt in cannot expose resource metadata
or bodies. Prompt and instruction exposure policy still has no model-facing
reader. `mcp_connect` plus `process_spawn` are mandatory package permissions,
and credential-reference environment entries also require `credential_use`.
Transport, generated tools and teardown remain owned by the standard MCP
connection effect.

## Bounded reconnect supervision

`McpReconnectSupervisor` is the single lifecycle owner of one server's
recovery. It sits above a transport rather than inside one: a transport
implements `McpReconnect::connect`, which re-establishes its own channel and
completes its own `initialize`, and the supervisor decides only whether and
when to ask for another.

**A crash loop exhausts; it never spins.** The attempt budget and the backoff
are the definition's own `McpReconnectPolicy`, never constants buried in the
supervisor. Attempt 1 is immediate and every later attempt waits
`min(initial_delay_ms << (n-2), max_delay_ms)`; with the workspace default
(`500 ms` initial, `30 s` ceiling, `10 attempts`) the schedule is
`0, 0.5, 1, 2, 4, 8, 16, 30, 30, 30` seconds, so a full exhaustion costs at
most ~91.5 s of wall clock and exactly ten connects. Exhaustion **retires the
rows first, then reports** `ReconnectExhausted` with
`McpGenerationRetention::Remove`: a snapshot that claims no retained
generation must not leave that generation's tools model-visible. Exhaustion is
then **terminal for the supervisor** — `recover()` answers `Exhausted` rather
than re-arming, because an outer Consumer that retried a spent supervisor would
rebuild exactly the crash loop the budget exists to bound.

**A recovery swaps exactly once.** A failed connect never reaches the
generation owner, so the previous rows stay whole and complete for the entire
outage — there is no window with zero rows and none with a partial set. Only a
successful connect reaches `refresh`, whose all-or-nothing swap advances the
registry generation at one commit point. Admission is single-flight: while an
episode runs, further `recover()` calls answer `AlreadyRunning` and start
nothing, so a burst of failure reports still produces one generation. A success
resets the consecutive-failure budget; a connect that survives but cannot list
still spends its attempt.

**Ownership.** The supervisor's token is a child of the caller's, it owns the
one retry task's `JoinHandle`, and `shutdown()` — runtime-free, idempotent and
non-blocking, because a context disposer cannot await — cancels the token and
aborts the task. `Drop` calls it, and the owning effect drops the supervisor,
so no attempt outlives its context. Cancellation is not a commit point and not
an exhausted budget: a cancelled episode publishes nothing terminal and leaves
the live rows alone.

Admissions and outcomes are closed enums carrying only counts and the published
generation number. The supervisor has no error type and no message strings at
all, so no endpoint, session id, header or response body can reach a log
through it.

**Mounted on the stdio path.** `stdio_definition` builds an enabled policy, and
every stdio connection arms a liveness slot that the driver loop fires when its
loop ends for any reason other than an intentional shutdown. The signal reports
`Transport` failure with `KeepLastGood` first — a `Degraded` row is the truth
whether or not recovery is permitted — and then asks the supervisor for one
bounded episode. `StdioReconnector` re-spawns the same definition through the
same routing plane and prompt catalog and swaps the live connection slot only
after the new child has handshaken and its sibling listings are complete. The
routing plane really is the same one: MCP11 elicitations, logs and progress from
the recovered child reach the product exactly as they did before the crash.

Streamable HTTP definitions keep a **disabled** policy on purpose: an HTTP
session has no persistent transport that can die between calls, so there is no
liveness event to arm a supervisor with and nothing to reconnect *from*.

## Stdio transport

The `mcp` plugin starts configured stdio servers through the shared sandboxed
subprocess service, performs the bounded initialize handshake and registers
qualified tools as `mcp__<server>__<tool>` through the generation owner above.
Its driver marks the list-change watch on `notifications/tools/list_changed`.
On shutdown the connection is killed before the registration handles drop, and
even a separately held `ToolRegistry` immediately loses the MCP rows. Stale
handles cannot remove a later same-name replacement.

## Streamable HTTP transport

`McpStreamableHttpClient` implements the **legacy era** of the transport,
revisions `2025-06-18` and `2025-11-25` — the era that establishes a session
with an `initialize` handshake. heycode targets it because that is what deployed
servers speak today and because the surrounding MCP07/MCP08 rows assume session
ids.

> **This is deliberately not the current revision.** `2026-07-28` is current and
> **removed** protocol-level sessions, the `Mcp-Session-Id` header, the GET
> stream and `Last-Event-ID` resumability, and replaced the handshake with
> per-request `_meta`. Do not read heycode's session handling as spec-current.
> <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http>

`McpProtocolVersion` is therefore an explicit closed enum: an `initialize`
result naming any other revision disconnects without sending
`notifications/initialized`, and adding the stateless era later is a new arm
plus its request shape, not a rewrite of this engine.

Wire obligations enforced:

- Every JSON-RPC message is its own POST to the single MCP endpoint, with
  `Accept: application/json, text/event-stream` and, for a body,
  `Content-Type: application/json`.
- A request response may be either one JSON object or an SSE response stream;
  the client branches on the response content type at runtime. Comments and
  keep-alives are ignored, and notifications on the stream are observed before
  the matching response.
- The `InitializeRequest` carries no session identifier and no protocol-version
  header. Every subsequent request carries `Mcp-Session-Id` (when the server
  assigned one) and `MCP-Protocol-Version: <negotiated>`.
- HTTP 404 for a request carrying a session identifier means the server
  terminated it: the client starts a new session with a fresh
  `InitializeRequest` without a session identifier and retries once. A second
  termination fails instead of looping.
- `terminate()` sends DELETE with the session identifier and treats `405`
  (termination not allowed) and `404` (already forgotten) as success. A
  session-less connection sends nothing.

Security properties:

- The session identifier is credential-grade: `McpSessionId` validates
  1..=512 visible ASCII bytes (so it cannot re-enter a header as whitespace or
  a control byte), redacts its `Debug`, has no `Display` or serialization, and
  never enters the registry snapshot.
- Errors are bounded and body-free. `McpHttpError` retains only a status code, a
  JSON-RPC code and static requirement text — never a URL, header, session id or
  response body.
- The client never sends an `Origin` header. Origin validation is a *server*
  obligation in this transport; a non-browser client asserting a browser origin
  would only weaken it.
- `McpStreamableHttpClient::new` refuses credential-reference headers rather
  than connecting unauthenticated. `new_with_credentials` accepts only a
  complete safe binding set and resolves values at each outbound operation.

### The composed `http` service

The engine speaks to `heycode_http::HttpService` directly — there is no parallel
transport abstraction in this crate. It reuses the composed service's
URL/header validation, `HttpRequest::post`/`delete`, bounded response headers,
bounded body-free errors, `SseDecoder` and cancellation, and heycode-mcp builds no
HTTP client of its own.

`heycode_http::HttpResponse` implements `Debug` and carries response headers, so
this crate must never render one: the session identifier lives there. It is
lifted into `McpSessionId` at the boundary — where an id outside the
visible-ASCII contract is refused rather than echoed into a request header —
and nowhere else.

Tests double the same seam: `tests/it/streamable_http.rs` scripts a
`heycode_http::HttpTransport` so protocol contracts run deterministically on the
production path, and `tests/it/streamable_http_service.rs` drives initialize, a
two-page list, a call, cancellation, session echo, DELETE termination and a
malformed-session refusal over a real socket through `ReqwestHttpTransport`.

## Deliberately not implemented

- **GET-stream resumption** (`Last-Event-ID`) and the server-initiated GET
  stream. Revision `2026-07-28` removed both, together with sessions; a broken
  response stream loses its in-flight request and must be re-issued. Recovery
  is a whole new session under the supervisor above, never a resumed stream;
  resources/subscriptions are MCP08's.
- **The modern stateless era** (`2026-07-28`): per-request `_meta`,
  `server/discover`, `subscriptions/listen`, MRTR input requests and the
  `Mcp-Method`/`Mcp-Name`/`x-mcp-header` request-metadata headers.
- Sampling and roots server→client requests remain unimplemented. Elicitation,
  progress and logging are product-composed; OAuth, resources, prompts and
  management live in their dedicated modules. O09 still needs concrete
  structured handler providers beyond its durable lifecycle adapter.
- Streamable HTTP endpoints declared in `heycode.toml` now connect through the
  product plugin, which owns the HTTP service; the PL04 bridge does not, so
  there an HTTP entry stays an inspectable definition with no connection. Provider-specific MiniMax/Z.AI bound
  bundles remain separate factory/live-evidence work.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-mcp --all-targets -- -D warnings
cargo test -p heycode-mcp --no-fail-fast
```
