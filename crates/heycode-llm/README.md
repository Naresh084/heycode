# heycode-llm

`ProviderRegistry::activate(provider, model, cancellation)` is an explicit host
activation boundary. `ProviderActivator` proves and constructs an independently
configured target; installing it does no discovery or inference. Existing
providers are preserved. Its effect-owned registration removes its late
providers on disposal and prevents in-flight construction from publishing after
revocation. Routing persistence and automatic-fallback permission remain outside
this API.

Provider-neutral inference vocabulary, strict provider adapters, model catalogs,
retry/error facts and token measurement. Provider implementations supply exact
wire and catalog evidence; this crate never promotes protocol compatibility or
missing metadata into model support.

`InferenceAdapter::reasoning_effort_options` is the read side of the exact
request contract. It returns provider-owned `ReasoningEffortId` values in
display order plus an optional default only for a model with explicit reasoning
support. Empty, duplicate, malformed and out-of-list defaults fail before UI
publication; unsupported or Unknown models expose no picker. Protocol wrappers
forward the same route metadata they use for resolution, so discovery cannot
drift from the value ultimately serialized and retained in `ResolvedCall`.

## Conformance fixture provenance

`heycode_llm::testing` owns the reusable CAT08 fixture boundary. Every SSE fixture
and generated fragmentation case carries validated provider, source kind,
credential-free HTTPS source, source version and non-zero capture/reconciliation
time; `ConformanceRun` preserves that metadata beside the normalized result.
Synthetic protocol examples are labelled synthetic rather than masquerading as
provider captures.

`CatalogConformanceFixture` is a strict schema-v1 JSON envelope with the same
mandatory metadata and an object/array provider payload. It is bounded to 32
MiB, rejects unknown fields and unsupported schema versions, and round-trips
without giving payload bytes a way to mint their own provenance. Provider
catalog tests consume the payload only after this admission step.

## Per-operation credentials

Authenticated routes hold a validated non-secret `CredentialHandle`, not a
captured registry value. `RouteCredential::acquire` resolves exactly once when
an operation begins; every retry of that operation shares the acquired value,
and the next request resolves again so environment/keychain/file/command
rotation is immediate without recomposition. A resolver permanently binds one
route and refuses a foreign handle before consulting the registry, so an absent
OpenAI key can never fall back to an Anthropic or gateway record.

The binding preview is durable-safe: registry-backed routes publish
`AuthenticationBinding::Credential(reference)`, while an explicitly
owner-supplied fixed credential publishes `AdapterOwned`. Credentialed Chat,
Responses, Anthropic Messages, Gemini and Bedrock adapters all acquire through
the same boundary. A deliberately unauthenticated Chat route instead publishes
`AuthenticationBinding::None` and emits no authorization header; absence is an
explicit adapter configuration, not a failed lookup. Construction, errors and
Debug expose no value; a missing/unavailable configured route is an
authentication-class failure before transport.

## Caller-owned provider readiness

`Provider::prepare_inference` is the only asynchronous route-discovery phase.
Agent invokes it after exact catalog/model/N01 selection and before borrowing
the operation adapter, materializing options, running P10 or committing C02/C05.
The caller owns and joins its cancellation token. Ordinary providers return
`None`; a lazy cloud provider returns one exact prepared `Arc<dyn Provider>`
whose endpoint, descriptor and model must match the selected evidence.

AWS uses the phase to force private live callability evidence that cannot fit
in `ModelDescriptor`; Vertex uses it to resolve the composed GCP profile and
construct the exact endpoint. Catalog and inference credentials remain
independent operation acquisitions. Discovery inside `stream`, a blocking
nested runtime or a detached preparation task would violate the durable target.

## Catalog metadata provenance

Non-empty `ModelPricing` and observed `ModelPerformance` carry a validated
`ModelMetadataProvenance` at the value layer itself: a safe exact source label
and non-zero Unix-millisecond capture instant. Unknown metadata carries neither.
An enclosing catalog revision/timestamp cannot substitute for this provenance,
because pricing or a benchmark result may be copied, cached or displayed apart
from the generation that transported it.

Pricing/performance remain advisory. Request resolution reads lifecycle,
capabilities and limits only; changing a price, benchmark source or capture
instant cannot change the resolved provider request.

## Native compaction boundary

`InferenceAdapter::native_compaction` is optional and independent from catalog
capability evidence; a Consumer requires both. The operation consumes an exact
`ResolvedCall` whose purpose/native feature identify compaction and returns a
bounded `NativeCompactionCheckpoint`. Every item must validate and share one
provider/model/protocol route. The provider owns transport only—session mutation
belongs to `heycode-agent` at its verified commit point. Failures are closed,
body-free classes and caller cancellation must settle the operation.

## Request envelopes

C11 constructs `TokenEnvelope` from production request planes:

- strict routes measure the actual `ResolvedCall`;
- compatibility routes measure the actual `ChatRequest`;
- transcript Messages may use the best effect-registered exact counter;
- System, Tools and lossless ProviderState use the explicit local estimate,
  because an endpoint cannot isolate those contributors without fabricating a
  transcript;
- native Attachments remain uncounted until a modality-aware counter exists;
- better-ranked counter refusals remain attached to the contributor.

The total is exact only when every non-zero contributor is exact. An estimate
weakens the total; an uncounted contributor produces an `AtLeast` lower bound.
There is no accessor that turns unknown into zero.

`ContextProjection` retains better-ranked counter refusals on each contributor
so a human surface can show that a fallback occurred instead of presenting only
the eventual number.

Provider endpoints may also be estimates. `ProviderTokenizer` is ranked ahead
of the local `Utf8ByteRatio` method but still mints `EstimatedTokenCount`;
Anthropic `/count_tokens` uses this class because its current documentation
calls the result an estimate. Provider-operated never silently becomes Exact.

## Hidden experimental audio

ATT04 deliberately adds no audio field to ordinary catalogs or `ChatMessage`,
and no shipping provider overrides the default-absent path.
`ExperimentalAudioDescriptor` is always Hidden, binds one exact
provider/model/protocol/target, keeps input/output support independently
tri-state and allowlists exact MIME formats and bounds. A supported call holds
ordinary neutral text/tool messages plus hash-verified audio bound to user
message indexes. Pending output bytes use a non-serializable, redacted type and
a separate stream enum; Agent admits them through ATT01 only at terminal
success. Audio remains `Uncounted(Unmeasurable)`, never zero.

## Provider request options

Chat routes may configure several unique durable option dialects. A dialect can
map the whole option object to a top-level request field or unwrap one exact
sole member. Duplicate kinds/target fields, reserved collisions, missing
members and unprojected siblings fail before transport. OpenRouter uses this
for whole-object routing plus provider-owned `plugins` transforms; every
constructor requires an explicit transform decision so omission cannot inherit
gateway/account defaults.

`Provider::request_options_for` is the request-specific seam for options whose
validity depends on the catalog-selected model or exact N01 native-tool routes.
Its default preserves static provider policy; provider-native tool wrappers
override it and materialize only selected routes. The current Agent still calls
the legacy static hook, so root must pass the selected `ModelDescriptor` and
committed N01 routes before these options are product-reachable.

Gemini routes use `GeminiProviderOptionPlan` to bind one exact option to either
one `tools[]` member or explicit top-level fields. A fresh provider-owned
normalizer per attempt observes candidates, parts and cumulative usage only
when that exact plan/route was selected. Search and external grounding emit
call/result/citation facts; code execution emits correlated call/results; the
unchanged `Content.parts` (including code and thought signatures) remains the
replay source. Explicit `cachedContent` usage emits neutral detailed cache
metadata without adding cached tokens twice. Implicit caching sends no field
and does not invent an absent write counter.

Anthropic-compatible routes may select an `AnthropicMessagesDialect`. The
native dialect appends `/messages`, includes `model` in the body and can send
the version header. An exact-model dialect pins a full endpoint, rejects another
selected model, omits `model`, and inserts validated provider-owned body fields.
Claude on Vertex uses this seam for `anthropic_version=vertex-2023-10-16` while
reusing the complete Messages parser, state and replay implementation.

## Hosted and server-tool normalization

Responses routes can register a provider-owned `ResponsesServerToolPlan`. One
exact, durable `ProviderRequestOption` selects an allowlisted subset of exact
`tools[]` definitions; the shared adapter splices only those definitions and
disables replay-unsafe retries. Each completed output discriminator has a
provider-owned `ResponsesServerToolNormalizer`, which returns bounded
`ServerToolCall`/`ServerToolResult`/citation facts while the parser always emits
the unchanged `ProviderStateItem`. State-only normalization is explicit for
client-executed or approval items such as computer use. Completed assistant
message `url_citation` annotations normalize from the full output item, never
from an opening placeholder. Replay correlation uses real provider ids, permits
completed historical tools without reauthorization, requires the exact current
definition for pending work and rejects unsettled provider-executed calls at a
successful terminal response.

Messages routes can register an exact `AnthropicServerToolPlan` selected by its
own durable provider option. A plan contributes versioned `tools[]`, beta values,
top-level request fields, deferred-tool names and exact call/result routes.
Routes can bind MCP server names and must carry a stable provider-owned
`AnthropicServerToolResultNormalizer`; this is the authority for web-fetch
sources, code/advisor/tool-search result unions and MCP `is_error`, while legacy
native-web definitions retain the narrower generic fallback. The parser
correlates server and MCP results across later responses, maps code subtool names
to one logical family, emits aggregate usage before ordinary Usage, and retains
the complete assistant block array for replay. Completed historical calls keep
their required beta headers without re-enabling their tools; only pending calls
require the same exact current plan. `pause_turn` remains distinct even when a
provider pauses at an iteration boundary with no pending server call.

These are protocol and replay boundaries, not new product selectors. The shared
`NativeFeature` enum still exposes only Web/Compaction/PromptCache because Agent
and request snapshots match it exhaustively. Non-web family selection, OpenAI
computer/approval/image attachment execution, durable session publication and
the capped Agent continuation loop remain owned by the root/session/Agent
integration.

OpenRouter Chat additionally accepts the current guide and generated-schema web
usage aliases (`server_tool_use` and `server_tool_use_details`), requiring equal
counts when both carry `web_search_requests`. A content-free terminal usage
frame may repeat the same `finish_reason`; conflicting reasons, late output and
non-usage repeats still fail. Perplexity search uses its documented 20-result
ceiling while other engines retain the broader provider bound.

## Strict provider interception

The `llm` plugin publishes service `provider-interception` and exact seams
`provider/request` plus `provider/response`. Plugin layers register as Context
effects and disappear on rollback/shutdown. Missing `next` is observable and
fails closed unless the layer supplied a validated lowercase policy code;
layer error bodies are discarded.

Request interception runs before adapter resolution over the C02/C05 durable
draft. Provider/model/catalog/input chronology is read-only; mutable methods
cover only system/tools/options and other fields independently persisted in the
request header. Every strict adapter also publishes a secret-free auth-binding
preview; the resolved call must match it. The adapter validates the edited
draft, Agent logs it, and C05 reconstructs and compares it before transport.
Response interception runs over each normalized strict event or only a
body-free `ProviderErrorClass` before telemetry, accumulator validation,
session append or UI publication. A refusal cancels the same provider operation
before the turn closes. Native subagents share the composed service. Legacy
Chat compatibility and distinct native-compaction operations are explicitly
outside this exact seam rather than being advertised as intercepted.

## Request transform registry

Optional plugin `request-transforms` publishes an effect-owned provider policy
registry and attaches one post-`next` P10 request layer. Provider descriptors
are validated and frozen at registration, sorted by exact id and removed by the
owning Context effect. Enabled rows must name explicit cost evidence; disabled
rows carry none, and Unknown never means documented free.

For a matching provider the layer inserts its exact option when absent, accepts
an equal durable option and refuses a same-kind conflict. It never overwrites
another policy decision. Adapter validation and C05 still own final transport
admission. OpenRouter contributes the current context-compression, file-parser
and response-healing rows from its provider crate; the generic registry has no
OpenRouter ids or wire fields.

## Detailed response facts and cache-aware cost

Strict adapters may emit one validated `InferenceEvent::ResponseMetadata`
before terminal Usage/Finish. OpenAI Responses normalizes detailed read/write/
reasoning counters when the current usage dialect is present (and requires it
for an explicitly configured cache call). Anthropic maps its provider-local
cache and applied-edit report without retaining raw provider objects.

`RequestCost::derive_detailed` prices a response only when uncached/read/write
form an explicit partition and every used component has published pricing. An
OpenAI-style cache subset with no non-overlap proof remains Unknown rather than
being subtracted or double-billed.

Bedrock Converse accepts one exact runtime-metadata option for route evidence,
cache checkpoints and guardrails. Cache read/write counters and bounded
per-TTL write details become detailed response metadata; `inputTokens` remains
the uncached partition and `cacheDetails` is never added on top of the aggregate
write count. The stream parser reconstructs one complete ordered assistant
message and emits `BedrockConverseMessage` before terminal usage/finish. Text,
tool input, opaque reasoning signature and redacted content replay verbatim only
for the exact provider/model/Converse route; wrong state fails before transport,
and Debug cannot expose the opaque data.

Chat server-tool usage may additionally emit `InferenceEvent::ServerToolUsage`
before ordinary Usage/Finish. This is an aggregate fact, not a call: the current
OpenRouter dialect preserves `web_search_requests` and emits no synthetic call
or result when the wire exposes none.

## Focused verification

```sh
cargo fmt -p heycode-llm -- --check
cargo clippy -p heycode-llm --all-targets -- -D warnings
cargo test -p heycode-llm --no-fail-fast
```

ConnectionProfile is setup metadata independent of a mounted ProviderProfile; local libraries may have no model default. Unknown capabilities remain unknown.

Connection profiles may also declare an ordered non-secret coordinate form and a distinct managed-cloud family. The generic onboarding shell renders those fields; providers retain validation of their meaning.

`CatalogRegistry::probe_endpoint` discovers a draft address through the provider without publishing a cache generation. It cancels with the caller or registration; default providers explicitly reject endpoint editing.

Endpoint discovery accepts an explicit operation credential only when the provider source advertises support. The registry retains registration cancellation and no-cache semantics; no credential is inherited from the active route.

`CatalogRegistry::probe_parameters` is the parallel draft-coordinate boundary. It calls only a provider that explicitly implements coordinate discovery, optionally carries one operation credential, validates the returned model generation, and never publishes that draft as the active or durable cached catalog.

Connection profiles may publish an exact adapter model set independently of catalog discovery. `admits_model` preserves that boundary without asserting availability or capabilities.


Azure deployments and custom OpenAI endpoints explicitly attempt ordinary
function-tool requests when tool capability evidence is Unknown. The catalog
continues to report Unknown, including after a successful call. Explicit
Unsupported still rejects locally; other unknown capabilities and other
provider routes retain strict evidence requirements. Requests preserve their
tool schemas and durable tool-result replay, and endpoint failures surface
without removing tools or switching protocols.

## Portable native-tool protocol transport

Chat routes can explicitly attempt Unknown function-tool support while retaining
Unknown catalog evidence; explicit Unsupported still fails before transport.
The compatible hosted gateways and MiniMax opt into that policy. Native feature,
vision and reasoning admission are unchanged.

Gemini declarations use `parametersJsonSchema` for the native tool schemas,
which contain JSON Schema unions and object constraints. Sending those as the
narrower Google `parameters`/`Schema` message loses the protocol contract.
Source: <https://ai.google.dev/api/generate-content#FunctionDeclaration>.

Chat configuration has narrow provider-owned controls for MiniMax's scalar
`reasoning_split`, image detail spelling, and GLM preserved thinking independent
of effort vocabulary. Ordinary Chat routes retain their existing defaults.
