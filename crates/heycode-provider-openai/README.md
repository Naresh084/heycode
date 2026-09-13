# heycode-provider-openai

Provider-owned OpenAI product contributions.

POA01 supplies the auth/catalog/profile plugins around the reusable
`OpenAiResponsesAdapter` that P03 already implements. This crate never
re-implements the Responses or Chat Completions protocol; it owns the registry
name `openai`, a descriptor declaring both `OpenAiResponses` and
`OpenAiChatCompletions`, the provider default `gpt-5.6-sol` and the non-secret
credential reference `OPENAI_API_KEY`.

Plugin `provider-openai` owns `authorization_flow:openai-api-key` and registers
it as a Context-owned effect. `OpenAiApiKeyValidator` sends the documented
bearer header through the composed `HttpService`. Key acceptance and model
entitlement stay separate proofs: `GET /v1/models` shows the key is accepted,
and only `GET /v1/models/{model}` shows the configured id exists for it, so a
`404` on the second request is `model`, never `unauthorized`. A configured id
outside the unreserved URL charset fails at construction, before any request.

Plugin `catalog-openai` adds the effect-owned authenticated catalog source. It
resolves its credential at refresh time and reads `GET /v1/models`. That
endpoint **accepts no query parameters and publishes no cursor**, so one
refresh is exactly one request to the bare path — sending a `limit` or an
`after` would be inventing a parameter the API never published. One
all-or-nothing generation is produced: a non-`list` envelope, a non-`model`
row, an unsafe id, a missing `created` or `owned_by`, an unparseable
`shutdown_date`, a duplicate id or an empty `data` array rejects the whole
generation, and a documented-shape body served under a non-JSON content type is
an interceptor rather than the model list. Status and transport diagnostics are
classified without exposing any response bytes.

The list is **account-scoped**: it is exactly what the presented key may use, so
the generation is the account-capable model set and it refreshes as that
entitlement changes. Unlike the Anthropic source, a generation that omits the
provider default is published rather than rejected — an account with no access
to the flagship is a real account, not a malformed response.

`shutdown_date` is the only availability evidence this endpoint publishes. An
announced date becomes `Deprecated` with an exact retirement instant at the
**start of that day in UTC** — the earliest instant the announced date can
begin, so resolution never dispatches past it (CAT03 resolves the effective
phase against an explicit instant). Absent and `null` both mean "not
announced" and stay `Unknown`; they never become a `Stable` claim. Only the
documented bare `YYYY-MM-DD` form yields a deadline, so an RFC-3339 datetime,
a pre-epoch date or a free-text value rejects the generation.

Everything else stays Unknown because the endpoint publishes nothing else.
Three exact maintained facts are joined for `gpt-5.6-sol`: the current compact
reference names that exact model for `/responses/compact`, the GPT-5.6 guide
documents its prompt-cache controls, and the exact model page documents
Responses web-search support. Those fields are Supported so their production
Consumers can require affirmative catalog evidence; every other capability and
every other account-listed model remain Unknown. Protocol compatibility and
family-name matching never contribute evidence. The endpoint publishes no
context window, output cap, alias or display name, so those stay `None`/empty
and identity is the id itself. Documented model pages do state windows and
prices, but a docs table is not API pricing evidence: no OpenAI API endpoint
publishes per-token prices, so every row records
`ModelPricing::unknown()` and `ModelPerformance::unknown()`.

No rate-limit header spelling is declared. The official OpenAPI specification
declares exactly one response header, `Retry-After` on its shared
`TooManyRequests` response, which `heycode-llm` already reads without a
declaration under RFC 9110. No `x-ratelimit-*` name appears in that primary
source, so this crate overrides nothing and `Provider::rate_limit_headers`
keeps its empty default (GOTCHAS #148).

POA01's provider-owned auth/catalog contract is complete. Its authenticated
live evidence remains absent: no OpenAI credential exists on the build host, so
`live_official_catalog_lists_account_models_when_enabled` skips unless
`HEYCODE_E2E=1` and `OPENAI_API_KEY` are both set. Every other contract is pinned
against a fake `HttpTransport`.

## POA02 — inference wiring and stateless replay

POA01 shipped identity, auth and discovery but nothing that could dispatch.
`OpenAiProvider` closes that gap: it advertises the reusable
`OpenAiResponsesAdapter` through `Provider::inference_adapter`, joins the route
origin to the documented `/v1/responses` endpoint, and fails the legacy
`Provider::stream` path loud — once an adapter is advertised no legacy fallback
is permitted, and a silent legacy dispatch would bypass every replay contract
below. A configured model id is authoritative; only its absence falls back to
the provider default, because this catalog is account-scoped and a route is
routinely built for an id that is not the flagship.

The product/catalog `Provider::descriptor` still advertises both Responses and
Chat Completions because OpenAI supports two distinct generation endpoints.
The advertised `InferenceAdapter::descriptor` is route-exact and contains only
`OpenAiResponses`; Agent requires one exact non-Unknown protocol before it can
project or verify route state, so returning the broader catalog descriptor
there would make this otherwise configured provider unreachable.

Production construction binds the configured non-secret credential reference
through `RouteCredential`; it does not retain the value resolved during startup
preflight. Each Responses or native-compaction operation acquires that exact
route once, retries reuse the operation value, and the next operation observes
rotation without recomposition. The strict authentication preview therefore
records the configured handle rather than `AdapterOwned`.

The route selects `ResponsesContinuation::ReasoningAndPhase`, the strictest
requirement the adapter offers. The adapter always sends `store: false` — the
API's own default is `true` — so nothing is retained server-side and
continuation depends entirely on what the caller replays. Both halves of the
requirement are stated by the API:

- **Reasoning.** `encrypted_content` "is populated by default for reasoning
  items returned by `POST /v1/responses`", and "when streaming, use the
  completed reasoning item and its `encrypted_content` from the
  `response.output_item.done` event in subsequent requests. The
  `encrypted_content` in `response.output_item.added` may be incomplete. This
  is especially important when `store` is `false`." The blob is opaque: a
  distilled or re-serialized copy is not the same item.
- **Phase.** The current guide defines `commentary` for intermediate assistant
  updates and `final_answer` for the completed answer, and says manual history
  replay must preserve each original phase. Its GPT-5.6 stateless `all_turns`
  example explicitly retains every output item, including encrypted reasoning
  and assistant phase.

The provider default `gpt-5.6-sol` is past both lines, so both apply here.

The provider wraps the shared adapter rather than exposing it directly. That
last provider-owned gate refuses neutral assistant history, an arbitrary
non-empty phase, and a reasoning item whose encrypted content is empty. Only
exact Responses output items with `commentary | final_answer` phases can enter
this stateless route.

A miss is a **pre-dispatch failure**, never a warning. A request that drops
these items still succeeds, just worse, so nothing downstream would ever notice
the regression. In particular a replayed assistant *tool-calling* turn
expressed as a neutral `ChatMessage` is refused outright: that type has no
reasoning item, no item id and no `phase` field, so the path cannot carry what
the route requires and "fixing" its serialization is impossible rather than
merely unimplemented.

### What the tests prove

The continuation tests here and in `heycode-llm` hand-build the items they replay,
so none of them can see a field the *parser* dropped.
`a_stateless_tool_loop_replays_every_published_item_unchanged` closes that gap:
it drives a real three-turn loop against one route, and the items it replays
are the exact `ProviderStateItem`s the stream published. The assertion is on
the captured turn-three request body — `encrypted_content`, `call_id` and both
documented `phase` values survive at their exact positions, alongside
`store: false`. Every scripted `response.output_item.added` carries no
`encrypted_content`, so a parser publishing the opening item rather than the
completed one fails here rather than on the live service.

### Known limits

- **The requirement is per route, not per model.** A non-reasoning model
  selected on this route would have its legitimate replay refused. Making the
  requirement model-aware needs evidence this endpoint does not publish.
- **The retained-item checks still cannot prove completeness.** The
  `gpt-5.6` family defaults to `reasoning.context: all_turns`, whose documented
  requirement with `store: false` is to "preserve every output item, append the
  next user message, and replay the complete history". The adapter checks that
  a replayed tool call carries its turn's encrypted reasoning and that replayed
  assistant message items carry an exact documented `phase`; the provider also
  refuses all neutral assistant history and empty encrypted reasoning. It still
  cannot see an item that was dropped before it was handed over, because
  nothing in the next request says how many items the previous response
  produced. The route sends no `reasoning.context`: omitted means "the model
  determines the context mode", which is the honest choice where heycode has no
  evidence to override the model default with.
- **`include: ["reasoning.encrypted_content"]` is now documented as legacy.**
  The reasoning guide says the API "still accepts the legacy
  `reasoning.encrypted_content` value in `include` for compatibility, but
  doesn't require it", and the reference says the field is populated by
  default. Sending it stays correct — it is accepted, and the reference still
  documents it as what "enables reasoning items to be used in multi-turn
  conversations when using the Responses API statelessly" — but it is no longer
  the mechanism that makes stateless multi-turn work.

Protocol claims above were verified on 2026-08-29 against the official
OpenAPI specification (`store` default, `MessagePhase`, `ReasoningItem.
encrypted_content`, `Reasoning.context`, the `include` enum) and
<https://developers.openai.com/api/docs/guides/reasoning>.

## POA03 — hosted-tool definitions and completed-item facts

The official `gpt-5.6-sol` model page proves support for all seven POA03
families: web search, file search, code interpreter, hosted shell, computer use,
image generation and MCP. `OpenAiHostedToolKind` records that exact closed set;
every other model id remains `Unknown` because the account Models endpoint
publishes no per-tool capability fields.

`OpenAiHostedToolDefinition` owns minimal current request definitions. File
search requires bounded unique vector-store ids. Hosted shell uses a managed
container with network disabled and direct invocation only. Remote MCP requires
a credential-free HTTP(S) URL, no query/fragment, mandatory approval, and has no
field for inline authorization or arbitrary headers. Definitions are released
only after exact model capability admission.

Completed Responses output items are classified by their type-specific current
schema while retaining the complete `ProviderStateItem` unchanged. Web action
is optional and never becomes a synthetic empty input. File/code results keep
only exact counts; hosted shell correlates the separate `shell_call` and
`shell_call_output` by the published `call_id`, never the replay-item id.
Computer `status:completed` means only that a client action item is complete:
the application must execute it and return `computer_call_output`, so neither
item is misclassified as a provider-executed server-tool event. Image base64,
shell stdout/stderr and MCP output/error text stay only in exact state and never
enter safe `Debug`.

MCP has three distinct completed item families: `mcp_list_tools`, `mcp_call`
and `mcp_approval_request`. The first two may omit `status`; list/call facts use
their exact ids and retain only counts or a documented error discriminator.
The approval request is a client continuation boundary, not a completed call.
Completed assistant messages separately expose public `url_citation`
annotations; file/container citations remain exact provider state because the
neutral citation plane represents public URLs only.

`OpenAiHostedTools` is the provider configuration hook into the generic shared
Responses path. It mints one schema-v1 `hosted-tools` option containing the
exact selected definitions, registers each completed-item discriminator on
`OpenAiResponsesConfig`, and `OpenAiProvider::with_hosted_tools` rebuilds the
adapter with that plan without overwriting prompt-cache options. Resolve
rechecks every selected definition against the actual model. A provider
transport fixture crosses request serialization, four call/result families,
URL citation normalization, unchanged `ProviderStateItem` emission and exact
replay validation for web search, file search, code interpreter and hosted
shell. The standalone classifier can truthfully represent a local shell item
as a client action, but a configured hosted-shell plan rejects that mismatch
before provider state or `Finish`; it never silently degrades to state-only.

Hosted-tool intent is now request-specific rather than a static provider
default. Each family publishes the exact N01 implementation id
`openai:<logical>`, and `Provider::request_options_for` turns only matching
provider-owned routes into the durable option. A client or MCP winner emits no
OpenAI definition; a mismatched/unconfigured OpenAI route fails before the
request header. The generic Responses plan retains the full provider-owned
allowlist and normalizers, so any selected subset can cross the same exact
wire/parser/replay path. Static `request_options()` contains only policies such
as prompt caching and cannot silently enable hosted work during compaction or a
prefer-local request.

`OpenAiHostedToolProductPolicy::bridge_complete_zero_configuration()` is the
explicit product constructor and deliberately has no `Default` implementation.
It contains only web search, automatically provisioned code interpreter and
network-disabled hosted shell. Root can configure a Provider with one call to
`configure_openai_bridge_complete_hosted_tools(provider)` and register the
identical candidate set through the effect-owned
`openai_bridge_complete_native_tools_plugin()`. A drift/lifecycle contract
asserts the literal kind set, exact plugin inventory, provider option and
withdrawal from a separately held registry after Context shutdown.

The provider-owned `openai-hosted-tools` Settings namespace now extends that
same named baseline without reconstructing it. It is restart-applied and
wire-verified. Every configuration decision is present in the document:

- `file_search.mode` is `disabled|enabled`; enabled requires one to sixteen
  unique bounded `vector_store_ids`, while disabled requires the list to be
  empty;
- `remote_mcp.mode` is `disabled|configured`; configured requires one bounded
  label, a credential-free HTTP(S) URL, and the required visible
  `require_approval:"always"` value. There is no authorization/header/token
  field.

`OpenAiConfiguredHostedToolPolicy::resolve(settings, selected_model)` validates
the complete resolved snapshot and exact model evidence, extends the existing
Search/code/shell generation with file search when enabled, and retains remote
MCP metadata separately from the executable plan. The matching
`openai_configured_native_tools_plugin(selected_model)` keeps the existing
`native-openai` plugin id, registers the namespace, derives the same policy,
and effect-registers exactly the executable N01 rows. Partial, duplicate,
unsafe or credential-shaped input publishes neither a namespace generation nor
a candidate. Shutdown removes candidates before the namespace.

The zero-configuration baseline is admitted per family against the selected
model, so Unknown is absent rather than fatal: a selected model with no
hosted-tool evidence yields a valid policy with an empty executable plan, no
N01 candidate and an unconfigured provider, and the plugin still activates. The
namespace itself carries no model-bound refusal in its defaults, because every
configuration family is off there. Explicitly enabling file search or a remote
MCP server names a family the operator asked for, so Unsupported or Unknown
evidence for the selected model refuses that value outright — an unproven
capability is never advertised as supported.

The composition root must replace—never compose beside—the former
`openai_bridge_complete_native_tools_plugin()` factory with
`openai_configured_native_tools_plugin(selected_model)`, then construct the
route with exactly:

```rust,ignore
let hosted = OpenAiConfiguredHostedToolPolicy::resolve(&settings, selected_model)?;
let provider = hosted.configure(provider)?;
```

Those are the exact resolver/plugin calls; root must not parse vector-store ids
or rebuild N01 identities itself.

The remaining root and upper-loop boundaries are explicit:

- file search has a complete shared bridge and now has provider-owned Settings,
  but root has not yet selected the new settings-backed factory/resolver;

- computer use needs the application action/safety/`computer_call_output` loop;
- image generation needs Agent/attachment admission before an unbounded base64
  result can become durable rather than inline session data;
- credential-free remote MCP metadata is admitted, but
  `require_approval:"always"` still needs a durable approval response/pause
  owner. It therefore publishes no N01 candidate yet.

Definitions and classifiers for the unowned upper loops remain available, but
a `MissingSharedBridge` refusal prevents accidental activation. Root must
replace the existing `native-openai` factory implementation with the
settings-backed builder and call the resolver above; it must not reconstruct
definitions or N01 identities in the binary. POA03 still lacks that root
composition proof and a live tool turn.

Official sources:

- <https://developers.openai.com/api/docs/models/gpt-5.6-sol>
- <https://developers.openai.com/api/reference/cli/resources/responses>
- <https://developers.openai.com/api/docs/guides/tools-web-search>
- <https://developers.openai.com/api/docs/guides/tools-shell>
- <https://developers.openai.com/api/docs/guides/tools-computer-use>
- <https://developers.openai.com/api/docs/guides/tools-connectors-mcp>

## POA04 — native Responses compaction

`OpenAiCompactionClient` owns the distinct buffered
`POST /v1/responses/compact` operation. `OpenAiProvider` now exposes it through
`InferenceAdapter::native_compaction`: a resolved `CallPurpose::Compaction`
with exactly `NativeFeature::Compaction` is mapped to the buffered request,
while ordinary stream dispatch of that call fails loud. The provider removes
the Agent's generic output cap during resolution because the compact endpoint
has no such parameter, and exact `gpt-5.6-sol` model evidence plus the catalog
capability are both required before I/O.

The operation uses caller cancellation and returns only a strictly validated
`response.compaction` envelope. Transport, status and response-shape failures
collapse into the C12 body-free taxonomy and never carry credential, input or
provider-body text. The credential is held through the shared redacted route
credential rather than as a second printable provider string.

The response contract is intentionally lossless. Every returned user item and
the final compaction item become an unchanged `ProviderStateItem`; the opaque
`encrypted_content` and any future provider extension remain inside that state
and are excluded from `Debug`. Current detailed usage is checked before the
shared checkpoint receives its exact input/output totals. The fixture drives a
resolved native call, serializes and restores its returned items, then sends
the complete user-item-plus-compaction sequence through the production
Responses route. Both objects and extensions reach the next `store:false`
request unchanged.

Neither half of the compaction branch is replay-safe, so the route withholds
pre-output replay from both. The compaction operation commits a checkpoint, and
a continuation carrying compacted provider state would re-enter server-held
state on a second send — yet that continuation carries no native feature, so
the shared resolver, which proves replay safety from native features and
provider-executed routes alone, would otherwise hand it the standard replayable
policy. An ordinary turn with no compacted state keeps that policy.

Official source:

- <https://developers.openai.com/api/reference/java/resources/responses/methods/compact>

Production credential-backed composition now registers `OpenAiProvider` and
the C12 registry exposes `provider-native` while keeping portable compaction as
the default. Isolated real-composition proof constructs the route from an
owner-only test credential, verifies its exact operation-time binding and
performs no request; the provider transport
fixtures remain the wire evidence. Human strategy selection remains CMD07
rather than silently changing `/compact`. Current compaction request mapping
covers the text/tool/provider-state plane the catalog can actually admit; a
future OpenAI image/document capability promotion needs a shared reusable
Responses input serializer rather than a second media encoder here.

## POA05 — prompt-cache controls and detailed usage

`OpenAiPromptCacheControl` owns only the current GPT-5.6 Responses fields:
`prompt_cache_key` and `prompt_cache_options` with explicit `implicit|explicit`
mode and the currently supported `30m` TTL. Keys are bounded identifiers and
redacted from diagnostics. It can now emit one schema-v1 `prompt-cache`
`ProviderRequestOption`, model-gated before the option becomes durable. Only
the maintained `gpt-5.6-sol` evidence is marked Supported; account model ids
without exact evidence stay Unknown.

`OpenAiCacheUsage` deliberately does not collapse its provider-local view into
the shared two-counter `TokenUsage`. It retains input, cached-read, cache-write,
output, reasoning and total tokens as separate checked values and exposes a
visible none/read/write/read+write activity class. Native compaction parses this
shape before returning normalized totals to C12. This matters because current
OpenAI guidance prices cache writes differently and explicitly tells
applications to track both `cached_tokens` and `cache_write_tokens`.

Official sources:

- <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
- <https://developers.openai.com/api/docs/guides/latest-model>

The normal production route now consumes this policy. `OpenAiProvider` admits
the model-gated option as its exact route policy, the shared Responses adapter
requires the configured object to contain every declared member and projects
both top-level fields into ordinary `POST /responses` requests. Empty,
duplicate and reserved target mappings fail at adapter construction; missing
or extra option members fail before dispatch. A production-provider transport
test proves the key and options arrive together with `store:false`.

Detailed Responses usage now maps to neutral `ResponseMetadata` before the
ordinary Usage/Finish pair. Agent validates the detailed totals, commits
`assistant/response-metadata` only after Finish, and `/context`/`/usage` replay
read/write/reasoning facts after restart. Missing reasoning detail remains
absent rather than becoming zero; cache-aware cost remains Unknown without a
non-overlapping provider partition.

The provider-owned Settings boundary is now complete. `provider-openai`
effect-registers restart-applied, wire-exposed namespace
`openai-prompt-cache` with `enabled`, `prompt_cache_key` and exact
`implicit|explicit` mode. Its default is disabled with no key. Non-empty keys
must satisfy the bounded provider grammar and the shared credential-material
screen; `prompt_cache_key` is explicitly classified public for schema/UI wire
exposure, but that declaration cannot make a recognized credential project.
Shutdown removes the namespace with the provider plugin.

`resolve_openai_prompt_cache_policy(SettingsService)` returns the typed
`Disabled | Enabled(OpenAiPromptCacheControl)` restart snapshot, and
`policy.apply_to(OpenAiProvider)` is the only root-facing activation step.
Disabled returns the provider unchanged; Enabled reuses the already-tested
model-gated exact object-to-wire path. Root composition injects Settings into
the credential-backed LLM factory, resolves this policy after
`provider-openai` registers the namespace, and applies it only to the OpenAI
provider. Production composition tests cover the default-disabled and enabled
snapshots without issuing a provider request.

Current deterministic inventory: **67 tests** (4 unit + 63 integration).
Credential-gated catalog/hosted-tool live evidence remains outside this count
unless the explicit E2E gate and a trustworthy OpenAI credential are present.
