# Provider, model and delegated-runtime plan

## Provider doctrine

A provider is not a base URL plus an API key. It owns route discovery, authentication, model catalog, protocol, request defaults, continuation state, streaming normalization, usage, error classification, retry, rate-limit metadata and provider-native capabilities.

The provider layer has two top-level contracts:

- `InferenceProvider`: heycode owns the agent loop and calls a model API.
- `AgentRuntime`: an official/external coding agent owns the loop and heycode bridges its sessions and events.

Codex and Claude subscriptions implement `AgentRuntime`. OpenAI and Anthropic API keys implement `InferenceProvider`. A provider plugin may contribute both, but they remain separate capabilities.

## Core interfaces

### Catalog

```rust
#[async_trait]
pub trait ModelCatalog: Send + Sync {
    fn provider(&self) -> ProviderDescriptor;
    async fn refresh(&self, request: CatalogRequest) -> Result<CatalogSnapshot, CatalogError>;
    fn cached(&self) -> Option<Arc<CatalogSnapshot>>;
}
```

Catalog snapshots include revision, fetch time, source and model descriptors. Live refresh is single-flight, cancellable, TTL-cached and retains the last good snapshot on transient failure. A forced refresh surfaces the failure instead of silently returning stale data.

### Inference adapter

```rust
#[async_trait]
pub trait InferenceAdapter: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;
    fn resolve(&self, draft: RequestDraft, model: &ModelDescriptor)
        -> Result<ResolvedCall, ResolveError>;
    async fn stream(&self, call: ResolvedCall) -> ProviderStream;
    async fn count_tokens(&self, request: TokenCountRequest)
        -> Result<TokenCount, ProviderError>;
    fn classify_error(&self, response: &ProviderFailure) -> ClassifiedError;
}
```

`count_tokens` advertises exact, estimated or unavailable. It is not faked as exact.

Portable Web results are external data, not provider instructions. WEB05 makes
that a typed Tool contract: successful search/fetch output commits
`untrusted_content:{source:web}` on `tool/result`, then request projection wraps
the exact text in a fixed data-only warning. TUI and native runtime visibility
derive from the same durable fact; provider switching cannot erase it.

Implemented P01/P08/C11 boundary: `InferenceAdapter::{descriptor,resolve,stream_cancellable}`, `RequestDraft`, `ResolveSpec` and ownership-consumed `ResolvedCall` exist. Common resolution validates route/model alias/protocol/lifecycle, target/auth metadata, tools/modalities/capabilities, exact reasoning ids, structured output, native features, temperature and output limits before transport. `ResolvedCall` owns bounded retry/replay-safety policy; body-free failures retain only stable semantic facts. Agent refreshes exact catalog evidence, logs header/context, independently projects/verifies and dispatches an advertised adapter without legacy fallback. The same resolved call now produces a five-contributor exact/estimated/uncounted token envelope before dispatch; compatibility calls use their actual `ChatRequest`. Better-ranked counter refusals and unmeasurable media remain visible rather than becoming zero.

ATT01 supplies validated content-addressed bytes plus durable MIME/name/dimension
metadata below every protocol. ATT02 now records an exact `user/attachments`
selection immediately before its user message, rereads and verifies those bytes
and requires both a strict adapter and selected-model `image_input=Supported`
before admission. Responses serializes text plus `input_image` data URLs; Chat
serializes text plus `image_url` data parts; Anthropic serializes base64 image
blocks before text. The accepted media set is PNG/JPEG/GIF/WebP, up to sixteen
images and 32 MiB each. Other protocols, legacy adapters and Unsupported or
Unknown catalog evidence refuse before the selection/message pair is written.
WEB03 supplies bounded portable HTML/PDF extraction and durable source
provenance. ATT03 adds separate `document_input` evidence and an explicit
durable route. A strict adapter plus exact Supported sends admitted PDF bytes
natively: Responses `input_file`, current Chat `file`, or Anthropic base64
`document`. Otherwise PDF/HTML runs through the composed 64-KiB bounded local
extractor, commits the derived `text/plain`, and records source→selected as
Extracted. Catalog or adapter changes cannot alter replay. OpenRouter maps the
public `file` input modality independently; current GLM-5.3-Flash advertises no
file modality, so it uses the honest extraction path.

### Agent runtime

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn descriptor(&self) -> &AgentRuntimeDescriptor;
    async fn account(&self, cancel: CancellationToken) -> Result<AccountState, RuntimeError>;
    async fn models(&self, cancel: CancellationToken) -> Result<CatalogSnapshot, RuntimeError>;
    async fn start(&self, request: RuntimeStart, cancel: CancellationToken)
        -> Result<Arc<dyn RuntimeSession>, RuntimeError>;
    async fn resume(&self, request: RuntimeResume, cancel: CancellationToken)
        -> Result<Arc<dyn RuntimeSession>, RuntimeError>;
    async fn fork(&self, request: RuntimeFork, cancel: CancellationToken)
        -> Result<Arc<dyn RuntimeSession>, RuntimeError>;
}
```

`RuntimeSession` supports send/steer/follow-up, cancel, permission and question responses, compact when available, event streaming and quiescent close.

Implemented R01 publishes this contract and the effect-owned `runtimes` registry. Descriptors carry explicit native/delegated loop ownership and tri-state operation evidence; account state is credential-free; ids and errors are validated/redacted. Every operation is caller-cancellable, and close means quiescent teardown rather than dropping a handle. Implemented R02 validates raw subscriptions/replays through one poisoned-on-failure normalizer: contiguous session/turn/tool/request identity, bounded safe payloads and settlement precede projection; live EOF fails and quiescent finite replay EOF succeeds. Usage may repeat per inner model step.

Implemented A01 registers the existing Agent as native runtime `native`. It exposes current-session start/resume/send/cancel/compact/close and the selected provider's catalog; unsupported fork/steer/follow-up/runtime questions/permission answers are advertised as Unsupported. Its bounded event hub derives model/tool/turn facts from post-commit Session events, while permission/notices use UiEvent. A04 gives each native turn a fresh cancellation lease. R03/R04/R06 register the pinned Codex health/catalog/primary-session boundary and R07/R09 register Claude Code health/primary stream sessions. R05/R08 adapt both delegated runtimes into fresh one-shot subagent providers with parent permission/cancellation ownership. OpenCode remains R10.

## Wire protocols

Implement protocol adapters independently from branded provider profiles:

| Protocol | Current consumers |
|---|---|
| OpenAI Responses | OpenAI, compatible gateways, Bedrock Mantle, LM Studio |
| OpenAI Chat Completions | OpenRouter, DeepSeek, MiniMax, Z.AI, legacy/local endpoints |
| Anthropic Messages | Anthropic, MiniMax, DeepSeek, Claude on Bedrock/Vertex |
| Gemini GenerateContent | Vertex Gemini |
| Bedrock Converse | Broad Bedrock model catalog |
| ACP | OpenCode and external agent runtimes |
| Codex app server | Codex subscription/runtime |
| Claude Agent SDK stream | Claude subscription/runtime |

Shared transports own HTTP/SSE/WebSocket mechanics. Provider adapters own request and state semantics. A shared transport never decides which reasoning fields or tool state must be replayed.

P13's WebSocket path is an optional connector seam rather than a provider
claim. It bounds ws/wss requests, opening frames, handshake reconnects,
cancellation, explicit HTTP fallback and outcome/aggregate metrics. No current
heycode inference protocol offers a WebSocket alternative; OpenAI Realtime and
Gemini Bidi require separate future adapters. A missing connector therefore
records an honest pre-connection fallback for server-to-client streams, while
bidirectional requirements and every post-connection failure remain terminal.

Implemented P02 transport boundary:

- `heycode-http` service `http` receives opaque URL/header/body requests and emits raw `SseEvent` values or transport errors. Header values have no Debug surface.
- Reqwest/rustls execution owns cancellation, non-success status/body-tail bounds, success `text/event-stream` validation and network/body failures.
- SSE framing is invariant under every byte split, including multibyte UTF-8; it handles BOM, line-ending variants, comments, ids, retry and multi-data events with a bounded buffer.
- Chat Completions alone recognizes `[DONE]` and OpenAI-compatible JSON. Existing DeepSeek/OpenRouter behavior is fixture-identical after injection of the default `http-reqwest` service.

Implemented P03 Responses boundary:

- `OpenAiResponsesAdapter` serializes one `ResolvedCall` and emits the normalized `InferenceEvent` stream. It is reusable protocol code, not an activated OpenAI provider/profile.
- Inputs are one chronological list of messages and route-compatible provider items. Stateless/ZDR output items replay losslessly; encrypted reasoning, function `call_id`, unknown output item types and `phase` survive.
- Response/item lifecycle, event sequence and identity are checked; text/reasoning/function deltas normalize while completed items also emit provider state. Terminal usage precedes Stop/ToolCalls/Length.
- Failed/cancelled Responses, sequence gaps, argument mismatch, unfinished items and EOF-before-terminal are protocol errors. No current v1 session consumer is allowed to discard this state; activation waits for C01 and POA01.

Implemented P04 Chat boundary:

- `OpenAiChatCompletionsAdapter` serializes one resolved call and normalizes one assistant choice with text, `reasoning_content`, parallel function calls, usage and terminal reason.
- Request reasoning uses explicit per-route `None`, `reasoning.effort` object or scalar `reasoning_effort` dialect. Optional `ChatThinkingConfig` adds a validated disabled id, complete canonical→wire map and omission policies. Shared Chat code never guesses from provider name.
- Completed assistant content/reasoning/tool calls become one lossless Chat provider-state message. First-fragment call identity and final reassembly are strict; late/multiple/unfinished protocol shapes fail.
- DeepSeek/OpenRouter now expose `InferenceAdapter` in addition to their working legacy interface. The compatibility projection retains user-visible behavior but intentionally cannot replay new provider state until C03/C04.

Implemented P05 Anthropic Messages boundary:

- `AnthropicMessagesAdapter` owns native-key/bearer headers, API version/beta metadata, exact disabled/adaptive/manual thinking + wire effort and versioned server-tool definitions. PAN01 still owns product auth/catalog/profile activation.
- Requests replay complete ordered `AnthropicMessage` content arrays and continuation containers. Parallel client results are one immediate User content array; durable `is_error` becomes the exact tool-result bit. Pending client/server tools require the same definitions.
- Streaming preserves thinking/signatures, redacted thinking, citations, client/server/programmatic tools, compaction, containers and unknown complete blocks. Model/lifecycle/index/tool identity, cache-inclusive monotonic usage and terminal order fail closed; provider-controlled discriminator strings are absent from errors.
- `pause_turn` remains distinct `FinishReason::Pause` with exact state. N02 now starts a new durable request/step only when a strict adapter committed that state, carries pending server-call correlation into the next parser and requires later settlement; legacy/state-free Pause fails and eight consecutive continuations cap a remote loop.

Production OpenAI and Anthropic hosted/server-tool plans are Settings-backed
generations. Their provider request options and N01 candidates resolve from the
same committed snapshot; no binary table or hidden fallback can make them
drift. File/advisor/tool-search/MCP metadata is admitted only by the provider's
strict schema, while computer/image/remote-approval loops remain unavailable
until their upper action owners exist.

AWS Converse and Google Developer/Vertex likewise resolve provider-owned
restart Settings during plugin activation. The wrappers retain static
provider/catalog inventory and contribute only configured Google native rows.
Settings model lists are policy allowlists, not capability evidence: an Unknown
catalog fact stays Unknown and verified request resolution refuses it. Mantle
selects exactly one explicit Responses or Messages dialect; PAWS05 is complete,
while cache/guardrail/grounding/code hosted evidence remains independently
active where the tracker says so.

C02 durable request substrate exists: `request/header` stores the exact resolved route/protocol/target, secret-free auth binding, full system text+SHA-256, tool schemas and options/purpose; `request/context` stores capacity and catalog generation evidence under the same `RequestId`. C03 persists every lossless Responses/Chat/Anthropic state item with matching route/protocol/schema. Tool-result errors remain neutral/durable through request projection. Provider adapters do not write session events themselves. C04/C05 map/verify them at the one loop dispatch point.

C04 protocol-aware projection exists. Same-route Responses/Chat/Anthropic state reconstructs chronologically and replaces only a complete duplicate assistant representation; partial reasoning keeps neutral fallback. Producing route/step/index settlement and context correlation are strict.

C05 verified dispatch now exists in `heycode-agent`. It compares independently projected durable route/prompt/tools/options/defaults/context/catalog/inputs with the live call and binds success to the exact adapter instance. Provider profile activation must use this gate; the legacy compatibility loop is explicitly not evidence that every turn is verified.

## Model descriptor

Implemented CAT01 vocabulary: every capability is `Supported | Unsupported | Unknown`; only Supported converts to true, only Unsupported to false, and Unknown remains `None`. `ModelDescriptor::unknown(id)` preserves identity with unknown limits/capabilities. `ProviderDescriptor` records stable id/display name/protocol families. Provider trait defaults preserve identity and declare protocol Unknown; shipped DeepSeek/OpenRouter adapters declare only `OpenAiChatCompletions`. They deliberately do not infer model tools/reasoning/native features from protocol compatibility. Provider-owned live sources replace Unknown field-by-field only when their endpoint supplies evidence.

Required fields:

- Exact model id, display name, aliases and provider route.
- Lifecycle: stable, preview, deprecated, retirement time.
- Context window and output cap, each optional when unknown.
- Input/output modalities.
- Tool calling, tool choice and parallel-call semantics.
- Structured output/JSON schema support.
- Reasoning levels and wire mapping.
- Required assistant-state replay rules.
- Native server tools.
- Native compaction/context editing.
- Prompt caching and cache controls.
- Token counting.
- Streaming and cancellation.
- Pricing and performance metadata when authoritative.
- Source and revision.

Catalog values are advisory until request resolution. The provider may reject stale catalog data with a targeted refresh hint.

Implemented B09 setup projection:

- `ProviderProfile` is provider-owned safe metadata: registry name, the same capability descriptor, default model and an optional non-secret credential reference. Setup/picker consumers validate registry-name/descriptor identity and never infer these fields from a provider id.
- `SetupCatalog` combines the composed `providers` and `models` services. Catalog generations supply selectable model rows after effective-retirement filtering; live/fresh/stale provenance and warnings remain visible.
- If no usable catalog exists, setup shows an explicit provider-default fallback and only that unproven state accepts a custom model id. It never fabricates model capabilities or labels the fallback as live.
- The restricted setup world uses metadata-only provider implementations plus real catalog plugins. It cannot dispatch inference and creates no agent/session/tool/TUI/MCP capability.

Implemented CAT02 catalog kernel:

- Plugin `models` publishes `CatalogRegistry`; provider sources implement `ModelCatalog` and register as disposable context effects.
- Each provider has an independent monotonic successful revision and commit timestamp. Model rows are id-sorted; blank or duplicate ids reject the entire candidate generation.
- `PreferCache` returns within the explicit TTL and refreshes after expiry. `Force` bypasses fresh cache. Concurrent refreshes for the same provider make one source call.
- A failed ordinary refresh returns the immutable last-good snapshot as `StaleFallback` with a classified safe warning. A forced failure remains an error. No cached generation means failure.
- Caller cancellation does not cancel a refresh shared by other consumers. Source disposal removes the catalog, cancels the registry-owned operation and settles waiters.
- Provider-specific OpenRouter and local/cloud discovery plugins remain separate tracker tasks. PDS01 now supplies DeepSeek discovery. CAT04 persists every generation separately from the user's provider/model id selection.

Implemented CAT03 lifecycle and selection kernel:

- Lifecycle evidence is `Unknown | Stable | Preview | Deprecated | Retired`, with optional retirement Unix milliseconds and ordered replacement ids.
- Selection compares against an explicit instant. A reached retirement deadline is authoritative even when an immutable cached row still says Deprecated.
- Exact ids and provider-recognized aliases resolve to one canonical descriptor. Blank/colliding aliases invalidate a candidate generation rather than producing ambiguous routes.
- Missing configured ids and effectively retired ids/aliases fail with no transport call and at most five canonical alternatives. Provider recommendations come first; remaining stable, preview, unknown and deprecated choices follow deterministically. Retired choices never appear.
- A deprecated model before its deadline resolves with a warning carrying the retirement instant and alternatives. Unknown lifecycle remains selectable because absence of evidence is not proof of retirement.

Implemented CAT04 persistence:

- `CatalogPersistence` is a unique disposable whole-generation provider under `CatalogRegistry`. Restored snapshots retain provider-local revision and successful commit timestamp; the next live generation increments from the restored revision.
- Refresh commits for different providers share a persistence lane so a whole-file store cannot lose one provider through concurrent read/replace races.
- Durable save succeeds before the candidate enters in-memory last-good. Persistence failure follows normal refresh semantics: forced reads fail; ordinary reads may return the prior generation as visibly stale.
- Built-in `heycode-catalog-file` writes explicit schema-v1 JSON to `$HEYCODE_HOME/cache/models.json`, capped at 32 MiB, atomically replaced and owner-only on Unix. Symlinks and wrong file kinds fail loud.
- The cache contains safe descriptors, lifecycle/capabilities, revision and `fetched_at_ms`; it contains no selected route. `[llm]` supplies the composition base, while CMD02 persists user runtime/provider/model ids in `[settings.routing]`; both remain references resolved against current catalog evidence.

Implemented CAT05 query filters:

- Tool, image-input and reasoning filters match exact `Supported`, `Unsupported` or `Unknown` evidence; `Any` is the only non-constraining value.
- Lifecycle filters distinguish `Selectable` from `Stable` and retain explicit Preview/Deprecated/Retired/Unknown diagnostic views. Deadline evaluation uses the query's explicit instant.
- Filter composition preserves deterministic catalog order and borrowed descriptors, providing the non-search filtering substrate for `U07` without copying or mutating generations.

Implemented PDS01 DeepSeek discovery:

- Built-in plugin `catalog-deepseek` registers one disposable source after catalog persistence and before LLM composition. It resolves the selected non-secret credential reference on each refresh, so credential rotation does not require recomposition.
- Authenticated `GET <base>/models` runs through the shared bounded buffered HTTP service. 401/403 classify as unauthorized, 429/5xx as unavailable and transport/shape failures retain their stable class; response bodies and keys never enter diagnostics.
- The complete response is validated before publication. V4 Flash/Pro receive the officially evidenced 1,048,576-token context, 393,216-token maximum output, Preview lifecycle and tool/reasoning/prompt-cache support. Unproven fields remain Unknown.
- Maintained tombstones record the 2026-07-24T15:59:00Z retirement of `deepseek-chat` and `deepseek-reasoner`, recommending `deepseek-v4-flash`. The resolver rejects either legacy configured id and returns the current alternative.
- No DeepSeek credential was configured on the development host, so successful live discovery is not claimed. The official endpoint returned the expected unauthenticated 401; deterministic HTTP/catalog fixtures and the gated future live lane cover behavior until credentials are supplied.

## Authentication types

Current API-key validation endpoints used by the S09 flow:

- OpenRouter: `GET https://openrouter.ai/api/v1/key`; 401/403 is unauthorized. If a model is selected, validate its slug against `GET /api/v1/models`.
- DeepSeek: authenticated `GET https://api.deepseek.com/models`; the current catalog must contain the selected V4 model.

Validation errors expose only stable classes (`unauthorized|host|model|network|cancelled`), never response bodies or submitted keys.

| Type | Examples | Storage/owner |
|---|---|---|
| API key header | OpenRouter, DeepSeek, MiniMax, Z.AI | heycode credentials |
| OAuth/browser | MCP, selected providers | heycode authorization flow |
| Device code | Codex | official Codex runtime |
| Subscription session | Codex, Claude | official runtime credential store |
| AWS SDK chain | Bedrock | AWS profile/environment/workload identity |
| Bedrock API key | Bedrock Mantle | heycode credential reference |
| Google ADC | Vertex | Google auth helper/workload identity |
| Local/no auth | LM Studio | endpoint probe |
| Command helper | enterprise gateways | executable credential provider |

S10 implements the command-helper provider contract. It resolves at operation time for immediate rotation, executes an exact argv vector without adding a shell, inherits only explicitly allowlisted environment names, enforces deadline/output/one-line UTF-8 bounds and emits fixed body/argv-free errors. E04 binds configured providers to the common subprocess/sandbox service, including descendant cleanup; the built-in instance intentionally contains no executable specs until project trust permits activation.

P09 applies that timing to inference. Production DeepSeek, OpenRouter, OpenAI
and Anthropic routes retain the configured non-secret handle after startup
preflight rather than a resolved value. Every operation performs one exact
registry walk; retries reuse that operation's value and the next operation sees
rotation. A resolver permanently binds one reference and refuses a mismatch
before registry access, so an absent route never falls through to another
provider/account. Strict durable auth snapshots name the configured handle;
owner-supplied fixed test/embedding credentials remain explicitly
`AdapterOwned`.

`/connect` may invoke the official client but never reads its token file. `heycode doctor` calls documented status APIs or commands.

## Logical native-tool routing

The model-facing logical capabilities are stable:

- `web_search`
- `web_fetch`
- `code_execution`
- `file_search`
- `computer_use`
- `image_generation`
- `remote_mcp`

Implemented N01 publishes `NativeToolRegistry` through plugin/service
`native-tools`. Provider, client and MCP plugins contribute validated candidates
as Context effects. At request resolution the registry deterministically selects:

1. Provider-native server tool when supported and allowed.
2. OpenRouter server tool or configured remote tool when the route provides one.
3. heycode client tool or MCP implementation.
4. Explicit unsupported result.

The initial policy is matching provider-native first, then client, then MCP;
priority and implementation id break ties. Provider candidates for another route
are ineligible. Built-in web Consumers contribute `client:web_fetch` and
`client:web_search` only when enabled. The sorted resolved choices are committed
in `request/header.options.native_tool_routes` and independently compared with
the live resolved call before transport. Schema v14 inserts the registry before
historical exact `tools`, `agent` or `subagent` consumers.

N04 adds configured `prefer-native`, `prefer-local`, `native-only`, or
`local-only` policy per logical capability through the
live `native-tools` Settings namespace: prefer modes cross families, only modes
fail before request commit, and local orders client before MCP. N02 owns durable
provider-native call/result/citation events; route policy never rewrites them.

Implemented N02 keeps provider correctness and inspection safety separate:

- Exact provider blocks, including encrypted search content/indices, remain lossless `ProviderStateItem` data and are the only model-replay path.
- Normalized calls retain logical/provider names plus bounded object input; results retain only outcome, count, safe error code and public HTTP(S) sources; URL citations retain bounded title/excerpt/range. Debug never prints input, URLs or cited text.
- Agent buffers exact and normalized output until terminal Finish, validates the complete candidate group through request projection before the first append and commits nothing on failure/cancellation.
- Anthropic `server_tool_use`, paired `*_tool_result` and URL citations emit normalized events. A result may settle a pending prior-request call after `pause_turn`, but orphan/duplicate/cross-route settlement fails. PAN01/PAN03 still own the shipping Anthropic profile/tool versions.
- POR05 and later provider tasks map their exact wire shapes into this boundary; N02 does not advertise provider tools by itself.

Provider-native results normalize into durable logical events while retaining provider-native ids, citations and state. The model history renderer uses the provider's required wire representation.

## Provider request/response interception

P10 publishes one effect-owned global service from every `llm` implementation.
`provider/request` operates only on strict C02/C05 calls: route/input chronology
is read-only, mutable fields are independently durable, auth preview must match
final resolution and native-tool routes receive registry admission before
transport. `provider/response` sees normalized events or only a body-free
failure class before telemetry/session/UI. Optional `provider-telemetry`
records closed provider/model/outcome dimensions through local-off or OTLP;
layer errors never carry the original provider body. Checked `next`, caller
cancellation and typed refusal make settlement explicit. Compatibility Chat
and provider-native compaction have different durable/result boundaries and do
not falsely claim interception here.

## Compaction strategy registry

Implemented C12 publishes effect-owned `compactions` with three rows. Strategies
prepare read-only plans and the registry alone commits one settlement after
checking the log did not change. Portable summary/prune use
`compaction/applied`; native state uses closed v2-only `compaction/native`.
Exact provider/model/protocol projection replays a native checkpoint, while an
incompatible route retains original history. The lower
`InferenceAdapter::native_compaction` operation consumes a resolved compaction
call and returns bounded same-route state; capability evidence and operation
availability are independently required.

| Strategy | Provider path | Durable result |
|---|---|---|
| OpenAI response compaction | `POST /responses/compact` | Production strict provider returns exact unchanged user+opaque checkpoint items and normalized usage |
| Anthropic server compaction | `compact_20260112` | Production strict provider merges beta/edit, normalizes the compaction stop and retains complete assistant state |
| Anthropic context editing | clear tool/thinking strategies | Applied-edit metadata; full local history retained |
| Codex delegated compaction | app-server `thread/compact/start` | Delegated runtime event/checkpoint |
| Claude delegated compaction | native Claude Code compact/autocompact | Delegated runtime event/checkpoint |
| Portable summary | any usable text model | Implemented bounded summary replacing one balanced durable prefix |
| Model-free pruning | any provider | Implemented explicit loss marker replacing the same balanced prefix |

Native opaque checkpoints are provider-owned. C12 never applies one to another
route. C14 now refuses direct persistence and exposes
`/provider <id> portable|fork|cancel`: portable settles before route commit,
fork creates a resumable child before native settlement with current route
unchanged, and cancel writes nothing.

Portable compaction records the summary and exact replaced boundary. TEL04 now
records closed committed compaction kind/provider/lineage/runtime metrics, and
CMD07 exposes the durable context/usage view. Neither invents instruction text
or convergence facts from a live operation.

## Provider continuation state

Some providers require state that is neither visible answer text nor disposable reasoning:

- DeepSeek thinking tool turns: full `reasoning_content`.
- MiniMax interleaved tool turns: complete assistant response/reasoning details.
- Gemini thinking models: thought signatures.
- OpenAI stateless reasoning: encrypted reasoning items when requested.
- Anthropic: thinking blocks and compaction blocks according to model/context strategy.

Adapters emit `ProviderStateItem` with provider, model, protocol, item kind, schema version and lossless JSON/bytes. Session projection includes an item only for an adapter that declares compatibility.

Provider state never appears in the generic UI unless the adapter supplies a safe renderer. Private reasoning is collapsed or hidden according to provider policy.

## Provider implementation requirements

The implementation-ready companion is the
[provider-authoring guide](../guides/provider-authoring.md); it maps this plan
to current traits, services, composition and acceptance gates.

### OpenRouter

Auth: bearer API key through heycode credentials.

POR01–POR06 status: provider-owned auth/profile and schema-v12 continuity ship; `catalog-openrouter` validates full and singular GLM metadata; typed routing is durable and reaches wire. Strict GLM dispatch exposes max/high/low with default max, preserves raw/structured reasoning state across tool results and refuses missing ingress/egress state. Plugin `native-openrouter` wins logical `web_search`, removes the client duplicate and sends the current model-invoked server tool with explicit bounded policy/budget. URL citations normalize through N02 and remain exact in Chat state. The production route also persists and sends an exhaustive transform decision: all known request plugins are explicitly disabled unless a future surface deliberately enables one, and response healing cannot activate on the current streaming route. N05 separately publishes those three rows through the shared request-transform registry, preserving requested/effective/effect/cost evidence and refusing any durable option conflict. Schemas v13–v15 preserve strict catalog/native contribution continuity. QLIVE01 now provides the credential-gated production catalog/text/reasoning/tool workflow and freshness artifact; POR04/QLIVE01 remain active until it produces a trustworthy authenticated pass, and POR05 independently awaits per-call web evidence.

N06 retains OpenRouter's documented aggregate
`usage.server_tool_use.web_search_requests` as provider-aggregate durable usage
with Unknown cost under the current `auto` engine. It remains distinct from
exact server call/result events; no query or call id is synthesized. `/usage`
therefore attributes the request count while POR05 stays active for its
independent exact-call/live evidence.

Discovery:

- `GET /api/v1/models` with pagination/filter/sort support.
- `GET /api/v1/model/{author}/{slug}` for validation.
- Preserve supported parameters, modalities, context, pricing and provider routing metadata.

Request features:

- Chat Completions baseline.
- Provider routing: order, fallback, parameter requirements, ZDR and data policy.
- Reasoning normalization.
- Tool and parallel-tool support from exact model descriptor.
- `openrouter:web_search` server tool and citations.
- Request plugins only through an explicit transform policy; response healing and context transforms cannot silently change requests.

Acceptance:

- `z-ai/glm-5.3-flash` live catalog and no-tool smoke; retain `stealth/ox-alpha` only as historical pre-release evidence.
- Tool-call round trip on a tool-capable model.
- Native/fallback web search with citations.
- Fallback routing and ZDR constraint tests.
- 401 invalid credential detected during `/connect`, before chat.

Current POR05 evidence boundary:

- Official request: `tools:[{"type":"openrouter:web_search","parameters":{...}}]`; deprecated `plugins:[{"id":"web"}]` and `:online` are not used.
- heycode resolves auto engine, 5 results, 3 uses, 15 total results, 4,000 characters per result and a top-level 5-call cap explicitly. Gateway web work disables retry.
- Official Chat stream exposes nested `url_citation` annotations and aggregate `usage.server_tool_use.web_search_requests`. The deterministic production-loader fixture validates both and commits the citation.
- No documented Chat field carries individual search ids/queries/results. heycode does not manufacture normalized call events; POR05 remains active pending authenticated raw evidence or an exact Responses item.

### DeepSeek

Auth: `DEEPSEEK_API_KEY` reference; configurable official base URL.

Discovery: official model endpoint where available, with a maintained fallback catalog carrying retirement metadata. Do not ship retired default model ids.

Current baseline:

- `deepseek-v4-pro` and `deepseek-v4-flash`.
- OpenAI Chat Completions and Anthropic formats.
- Thinking toggle and high/max effort mapping.
- Tool use with required reasoning-state replay.
- 1M context according to current official V4 documentation.
- JSON/strict tool behavior and optional beta FIM/prefix capability as separate tools, not chat assumptions.

PDS01–PDS03/B08 status: the authenticated catalog, V4 evidence, retirement advice/default migration and thinking controls/state guards are implemented. Exact choices are none/high/max with default high; enabled mode strips temperature and generic automatic tool controls. Replayed assistant tool calls require lossless Chat state with nonempty reasoning before transport, and new tool-call responses missing reasoning emit no valid provider state or Finish. PDS04/PDS05 cover the additional protocol and optional surfaces.

PDS05 completes the provider-local optional surface as four distinct contracts:
beta Chat strict tools, standard Chat JSON Output, beta Chat prefix and beta
`/completions` FIM. Ordinary tool schemas/prefix messages remain the durable
model-visible source while provider options carry activation only. FIM is
non-thinking and capped at 4K; Flash support remains Unknown because current
per-model sources conflict. PDS04 remains active for its explicit Anthropic-
format live smoke.

Acceptance:

- Thinking/non-thinking streamed text.
- Multi-step tool loop preserving `reasoning_content`.
- Unsupported sampling parameters removed in thinking mode.
- Retirement test rejects configured legacy ids with migration advice.

### OpenAI API

Auth: Platform API key or supported enterprise credential plugin; separate from ChatGPT subscription runtime.

Protocol: Responses first. Chat Completions is compatibility-only where needed.

Native features:

- Response streaming and state items.
- Hosted web/file search, code interpreter, shell, computer use, image generation and remote MCP by exact model/account capability.
- Tool search/deferred tools.
- Prompt cache keys/options.
- `/responses/compact`.
- Reasoning effort/summary and phase preservation.

Acceptance includes stateless and stored modes, ZDR-compatible state replay, compaction continuation and hosted-tool event normalization.

POA02 completes the stateless state contract at provider scope. The
provider-owned Responses route uses `store:false`, refuses neutral assistant
history, empty encrypted reasoning and phases outside
`commentary|final_answer`, and a three-turn transport fixture replays the exact
parser-published items/order/call ids/opaque reasoning/phase values. Current
official guidance explicitly says stateless `all_turns` must preserve every
output item, including encrypted reasoning and assistant phase. The crate is
now a credential-backed strict inference provider in production composition;
isolated activation performs no request and no live credentialed call is
claimed.

POA03 is active with provider-owned exact definitions/classifiers for web
search, file search, code interpreter, shell, computer use, image generation
and remote MCP. Model capability gates and completed-item preservation are
tested, but shared Responses event normalization/session projection and
hosted-tool activation remain absent.

POA04 is production-reachable through C12. The strict route resolves only an
exact compaction-purpose feature on evidenced `gpt-5.6-sol`, posts the distinct
buffered endpoint, maps failures to body-free classes and replays the complete
returned user+opaque item sequence unchanged into later `store:false` Responses
calls. POA05 is complete: effect-owned, restart-applied
`openai-prompt-cache` Settings default off, admit a bounded screened key plus
explicit implicit/explicit mode, and root resolves that generation before
provider publication. The exact option projects both required top-level
members through the normal Responses route, while detailed usage commits
through neutral v2 metadata into `/context` and `/usage`.

### Anthropic API

Auth: API key reference; separate from Claude subscription runtime.

Protocol: Anthropic Messages.

Native features:

- Tool use and parallel calls.
- Thinking/interleaved thinking preservation.
- Web search/fetch, code execution, advisor, tool search and MCP connector by model/tool version.
- Domain filtering is capability evidence, not a generic promise: the portable client seam enforces its own allow/block policy, while each native server-tool adapter must map supported forms or refuse the route instead of silently dropping rules.
- Token counting.
- Prompt caching.
- Server-side compaction and context editing.
- `pause_turn` server-tool continuation.

Tool versions are catalog data and can change without a binary release when compatibility is known. Unknown versions remain disabled until classified.

P05 provides the protocol adapter and fixtures for the exact evidenced shapes above, including parallel client-result batching, cache-inclusive usage, `pause_turn`, compaction and programmatic-tool containers. PAN01/02/04 now supply the credential-backed production profile, exact thinking continuation and native compaction; N02 adds normalized server events and native auto-continuation. PAN03 remains the server-tool product bridge.

PAN02 adds the provider-owned continuation policy. Complete `thinking` and
`redacted_thinking` blocks retain their opaque signature/data and exact block
order through every tool-result step. Adaptive mode interleaves without a beta
header and permits text-first turns; manual mode sends the versioned header,
uses a token budget and requires thinking first. Manual construction requires
an explicit model and rejects the known adaptive-only default, but arbitrary
model compatibility remains Unknown because the catalog publishes no complete
dialect/effort/interleaving inventory. Production inference registration and
native compaction now ship; live account evidence remains separate.

PAN04 is production-reachable through C12. A compaction-purpose call adds the
current beta/edit, disables replay, requires exact model evidence, normalizes
the provider compaction stop to Pause and returns one complete assistant
checkpoint. PAN05/PAN06 are complete: the effect-owned, restart-applied
`anthropic` Settings namespace defaults caching and context editing off, exposes
the complete explicit 5m/1h, thinking-retention and tool-clearing vocabulary,
and one resolved generation configures both inference and token counting.
Detailed response facts commit through neutral v2 metadata and usage UI.
`/count_tokens` is a higher-ranked `ProviderTokenizer` estimate, never Exact.

PAN03 is active with exact provider-owned definitions/classifiers for search,
fetch, code execution, advisor, tool search and MCP connector, including beta
extensions and pending `pause_turn` state. Shared Messages normalization,
durable event/UI projection and server-tool activation remain absent.

### MiniMax

Auth: separate pay-as-you-go and Token Plan credential profiles.

Protocols: OpenAI Chat Completions and Anthropic Messages, each tested separately.

Discovery:

- `/v1/models` for OpenAI family.
- `/anthropic/v1/models` for Anthropic family.

State:

- Preserve complete assistant response and reasoning state across tool turns.
- Apply model-specific temperature and unsupported parameter rules.

PMM03's provider-owned state boundary is complete at fixture scope. Routes are
bound to MiniMax product, canonical model and one of three dialects: native Chat
`<think>`, split Chat `reasoning_details`, or Anthropic-compatible ordered
thinking/text/tool-use blocks. Capture/replay rejects dialect mixing, duplicate
tool ids, cross-product/model state and incomplete reasoning. MiniMax's concrete
Anthropic-compatible response schema includes the opaque thinking `signature`,
which is required and replayed unchanged. The crate still has no inference
adapter/plugin; catalog capability remains Unknown and no live route is claimed.

PMM05 closes the current coding-profile boundary without inventing a renamed
third product. A Token Plan key selects coding use only with affirmative
assigned-seat or purchased-Credits evidence; the existing Token Plan provider,
credential kind/reference and documented `/v1|/anthropic` endpoints remain
authoritative. The former dedicated Coding Plan endpoint is Unknown because
current sources redirect to Token Plan rather than publish such a URL.

PMM04's provider-owned MCP bundle now follows the current Token Plan guide:
`AllDocumented` exposes both `web_search` and `understand_image`, while
`WebSearchOnly` is explicit least privilege. Both are prompt-gated and
untrusted; resources/prompts/instructions are disabled and local resource
delivery needs an explicit canonical directory. The row remains active until a
product MCP owner maps the credential reference, launches/connects the server
and observes both tools.

Native extensions:

- Token Plan web search and image-understanding MCP bundle.
- Coding-plan endpoint eligibility is an explicit provider profile and must follow current vendor terms.

### Z.AI / GLM

Auth: API key reference, with distinct general and Coding Plan profiles.

Protocol: OpenAI-compatible chat baseline; Anthropic-compatible route only when current official docs and contract tests prove it.

Discovery: provider catalog endpoint or maintained live-fetched directory. Model limits and capabilities come from current model descriptors.

Native extensions:

- Thinking mode and function calling.
- Web Search API/tool.
- Coding Plan web-search, web-reader, vision and Zread MCP bundles.

The Coding Plan endpoint is enabled only for an eligible tool profile. General API support ships independently.

PZA03's fixture route preserves complete Chat assistant state and function-call
fragments across multiple tool steps. `reasoning_effort` is sent only for the
canonical GLM-5.2/5.3/5.3-Flash ids Z.AI documents for that field; generic
reasoning capability does not enable it for older models. Older compulsory-
thinking rows still reject replay/response state missing `reasoning_content`
without sending an invented effort. The general endpoint remains degraded until
the shared dialect can express `thinking.clear_thinking=false`, and no Z.AI
inference plugin/live turn is claimed.

PZA04 completes standalone native web search at durable/product-registry scope.
The client validates title/summary/link/site/icon/reference/publication for the
whole result generation. N02 result sources carry all seven fields through a
real session reopen, and opt-in plugin `native-zai` registers exact N01 route
`zai:web_search`. The shared Chat parser still does not consume top-level
`web_search`, and no credentialed call or Z.AI inference registration is
claimed.

PZA05's four Coding Plan MCP definitions—search, reader, vision and Zread—are
validated, secret-free and transactionally registered through a host bridge.
The row remains active because shared Streamable HTTP and stdio connection
owners currently refuse credential references rather than resolving them at
launch; no connected tool generation exists yet.

### LM Studio

Auth: local endpoint and optional token.

Discovery:

- Native `/api/v1/models` for downloaded/loaded state and tool-training metadata.
- OpenAI `/v1/models` compatibility fallback.

Protocols:

- Prefer Responses when model/server supports required features.
- Support Chat Completions and Anthropic Messages profiles.

Local controls:

- Optional load/unload operation via explicit user command, never automatic surprise.
- Configure context length, GPU/cache settings through a provider settings panel.
- Server health/version and current loaded instances in `/status`.

Tool support is model-dependent and conservative. A loaded model that cannot reliably call tools can run chat-only but is not advertised as a coding-agent route.

PLM04 is product-reachable. Raw service `lmstudio/model-control` remains the
provider boundary; default plugin `lmstudio-control` contributes live Settings
namespace `lmstudio-load` and queued `/lmstudio <load|unload> <target>`. Load
rereads the latest user controls, admits only an affirmative tool-capable chat
model, verifies every echoed context/hardware field and exact instance readback,
then force-refreshes the shared catalog. Unload requires an observed instance
id and confirms it disappeared before the same refresh. Current LM Studio docs
expose global JIT only as a Server Settings switch, not a stable management API;
heycode refuses to edit private config and instead ensures only the explicit
command can invoke its load endpoint. This is deterministic product control,
not a real-install inference smoke.

PLM05 has a distinct Ollama product bridge: native identity/version/catalog
never reuse LM Studio facts, and native `/api/tags` alone remains protocol
Unknown. An explicitly configured provider/model conditionally mounts joined
version/tags/ps/show/OpenAI-list catalog evidence plus a no-credential Chat
provider into the ordinary registries, so route/model pickers see matching
descriptors. Composition performs no request, daemon start or pull, and a
credential reference is rejected before lookup. The row remains active only
for a fresh installed-model chat smoke; no Ollama executable exists on this
host.

### Amazon Bedrock

Auth profiles:

- Bedrock API key + region.
- AWS SDK chain with profile/region.
- SSO/workload identity through standard AWS configuration.

Discovery:

- Bedrock Mantle `/models` for Mantle models.
- `ListFoundationModels` and `GetFoundationModel` for Bedrock Runtime.
- Include region, inference type, modalities, streaming, lifecycle and endpoint compatibility.

Protocols:

- Responses/Chat Completions on Mantle where supported.
- Converse/ConverseStream as the broad native seam.
- Anthropic Messages for Claude routes.

Features:

- Tool use, prompt caching, guardrails, cross-region profiles and provider-specific request fields.
- Exact route identity includes account/profile, region, endpoint family and inference profile.

PAWS04–06 provider-local status: Converse stream/tool/cache fixtures, distinct
Mantle Responses/Messages providers and cache-checkpoint/guardrail/target
metadata are implemented. Shared Bedrock does not yet serialize those options,
retain detailed cache usage or gate selection on model streaming eligibility;
composition factories and the hosted PAWS04 smoke are absent. All three rows
remain active.

### Vertex AI

Auth: Application Default Credentials, workload identity or command helper. Project and location are required route fields.

Provider profiles:

- Gemini GenerateContent.
- Anthropic Claude on Vertex.
- Selected MaaS/open models only after dedicated compatibility tests.

Gemini state/features:

- Function calling and thought signatures.
- Google Search grounding and external search grounding.
- Code execution.
- Implicit/explicit context caching.
- Multimodal inputs and exact token counting.

Discovery combines configured project availability with a current publisher/model catalog; Model Garden listing alone is not proof of account access.

PGCP05–07 provider-local status: grounding call/result/citation projection,
code-execution events, cache provenance/usage and a conservative Claude-on-
Vertex profile/probe are implemented. Shared Gemini still rejects the required
options, shared Messages hardcodes its endpoint/body/header dialect, token
activation/factories are absent and no authorized live call ran. The rows remain
active rather than turning fixtures into product/live claims.

### Codex subscription runtime

Integration: official `codex app-server --stdio` or a pinned official SDK/runtime package.

Implemented R03 pins Codex CLI 0.146.0, binds the selected lexical shim plus canonical launcher and any sanitized `/usr/bin/env` interpreter, verifies version and initialize `userAgent`, parses strict raw JSONL with aggregate bounds, correlates bidirectional ids and settles cancellation/close through truthful containment. R04 adds exact pinned `account/read {refreshToken:false}`, provider capabilities and cursor-complete visible `model/list`; each operation owns/closes a fresh connection, discards account email, exposes only auth-source/plan labels and never reads tokens/auth files. R06 owns primary thread APIs. R05 starts the same runtime with `ephemeral:true` behind a fresh one-shot subagent and passed a tool-free installed subscription turn; deterministic fixtures prove permissions, plan/tool events, cancellation and durable final output.

Auth: official ChatGPT browser/device-code/API-key/account API. heycode currently reads account status and plan type through app-server methods, never auth files; login/logout/rate-limit UX remains later runtime work.

Runtime mapping:

- Account read ships; login/logout remain.
- `model/list` and `modelProvider/capabilities/read` ship with pinned normalization.
- Thread start/resume/fork and turn start/steer/interrupt.
- Permission, user-input and MCP elicitation requests.
- MCP startup/auth status.
- Context compaction items.
- Agent message/commentary/reasoning/tool/file-change/web-search events.

Both primary-runtime and fresh one-shot delegated-subagent modes now ship at
their accepted text/callback scope. Queued follow-up and attachment mapping stay
Unsupported under the pinned app-server contract.

### Claude subscription runtime

Integration: official Claude Agent SDK or Claude Code stream JSON using the installed/pinned native runtime.

Implemented R07 accepts the reviewed 2.x version interval, binds one executable identity across version/auth/query, reads only credential-blind official status and runs one strict text-only tool/MCP-free no-persistence canary. R09 owns current SDK-control stream sessions, lazy init, resume/fork/steer/follow-up/compact and permission/question callbacks. R08 adds the fresh one-shot subagent mode with host-minted identity, no-session-persistence/history, exact empty MCP configuration and disabled Chrome/slash commands. The installed tool-free delegated canary passed against Claude Code 2.1.251; deterministic fixtures prove plan/tool and allow/deny callbacks.

Auth: official Claude Code login/status/logout. heycode does not use `CLAUDE_CODE_OAUTH_TOKEN` unless the user explicitly configures it as an official automation credential.

Runtime mapping:

- Model/effort and permission mode.
- Session start/resume/fork and no-persistence mode.
- Partial messages, tool use, subagents, hooks and MCP events.
- Permission callbacks and user questions.
- Native compaction/context usage where the SDK exposes it.

Like Codex, it now exposes both the accepted primary-session boundary and a
fresh one-shot delegated-subagent provider. Interactive subagent questions are
refused rather than answered with invented input.

### OpenCode delegated runtime

Integration: OpenCode server/SDK or ACP, not credential-file copying.

Capabilities:

- Broad provider/model catalog.
- Sessions, agents, permissions, commands and MCP status.
- Streamed normalized events.

Use it for migration, interoperability and comparative testing. Native heycode provider plugins remain the preferred route when heycode owns the loop.

## Support tiers

| Tier | Meaning |
|---|---|
| Experimental | Adapter compiles; mock contract only; hidden by default |
| Preview | Live auth/catalog/smoke; incomplete native capability coverage |
| Supported | Mock + replay + live + tool + error + compaction tests; documented limitations |
| Certified | Supported on release platform matrix and continuous canary |

The UI shows tier and last verification date. “OpenAI-compatible” never grants a support tier by itself.

## Provider conformance tests

Q02 supplies the shared raw-SSE substrate: safe named fixtures, whole/bytewise/every-boundary fragment cases, explicit midstream transport failures, production decoding and a protocol-agnostic async adapter closure. Each case is isolated and records transport calls. Chat Completions and Responses are the first consumers; every new streaming protocol must reuse it.

Every supported route passes:

- Credential absent/invalid/expired and rotation.
- Catalog cold fetch, refresh, stale fallback, retirement and unknown fields.
- Text/reasoning streaming fragmentation.
- Usage ordering and missing usage.
- One and multiple tool calls, malformed args and parallel behavior.
- Full provider-state replay after tool results.
- Cancellation before dispatch, mid-stream and during native server tool.
- Context overflow classification and strategy recovery.
- Rate limit and retry-after.
- Transient 5xx/connection drop without duplicate committed work.
- Structured output success/failure.
- Image input when advertised.
- Native tool and fallback equivalence.
- Compaction continuation or explicit unsupported behavior.
- Session resume and provider/model switch rules.

Live tests are gated by explicit environment/config and record redacted provider, model, capability revision, latency, tokens and outcome.

## Current integrated cloud and hosted-tool routes

- OpenAI and Anthropic own explicit no-`Default` N01 product policies. The
  shipping zero-configuration sets are respectively Search/code/shell and one
  atomic Search/code plan; every configuration/action/media-dependent family
  remains gated.
- `llm.protocol` is schema-27 explicit. DeepSeek permits its established Chat
  route or guarded Anthropic Messages. Bedrock Mantle requires explicit
  Responses or Messages, and Messages requires a positive output-token default.
- Bedrock Converse/Mantle lazy providers force exact live provider evidence in
  the shared preparation phase. Public cache descriptors cannot substitute for
  streaming/on-demand/protocol evidence.
- Maintained Vertex Gemini and Claude catalogs publish exact current model facts
  while account access remains Unknown. Lazy inference resolves project,
  location and ADC health through the composed GCP service before C02/C05.
- Google Developer Search and code execution are production N01 candidates.
  Implicit cache was not enabled because the live Developer catalog does not
  prove prompt-cache support; Unknown remains non-permission.
- OpenRouter's POR07 workflow now combines fresh production text/tool evidence
  with search/routing policy in a content-withheld artifact. POR04/POR05/POR07
  and QLIVE01 remain active until a trustworthy authenticated run exists.
