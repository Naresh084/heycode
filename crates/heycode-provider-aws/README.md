# heycode-provider-aws

Provider-owned Amazon Bedrock product contributions.

Amazon Bedrock exposes two endpoint surfaces, and this crate registers one
CAT02 catalog for each, under two provider ids. Both endpoint surfaces run on
the Mantle inference engine; `bedrock-mantle` names an endpoint, not a different
engine.

| Provider id | Endpoint | Row | Protocols |
|---|---|---|---|
| `bedrock` | `bedrock.{region}.amazonaws.com` (control plane, discovery) | PAWS03 | Converse |
| `bedrock` | `bedrock-runtime.{region}.amazonaws.com` (inference) | PAWS04 | Converse profile implemented; the endpoint also supports compatible APIs |
| `bedrock-mantle` | `bedrock-mantle.{region}.api.aws` | PAWS02/PAWS05 | Responses and Messages profiles implemented; Chat Completions catalog evidence only |

The `bedrock` id covers two hosts, not one: discovery lists models against
the control plane, and inference dispatches against the runtime plane. One
provider identity, two endpoints, and the descriptor is defined once so they
cannot drift.

They are deliberately **not** one row. `CatalogRegistry` keys by provider id
and fails loud on a duplicate; the two endpoints host overlapping but different
model sets and name models in different namespaces. Mantle takes a plain
foundation model such as `openai.gpt-oss-120b`; the Responses API on
`bedrock-runtime` requires a cross-Region inference profile such as
`us.openai.gpt-5.6-sol`. Converse can instead accept a foundation model or an
inference-profile id/ARN. Nothing in this crate assumes an id from one catalog
resolves on the other.

Both catalogs read the effective region from PAWS01's `AwsAuthService` rather
than deriving a second answer, and both authorize through the same
`BedrockRequestAuthorizer` seam.

## PAWS03 — the runtime control plane

PAWS03 supplies runtime model discovery: one documented `ListFoundationModels`
call against the regional Bedrock **control plane**
(`https://bedrock.{region}.amazonaws.com/foundation-models`), normalized into
one all-or-nothing CAT02 catalog generation under the registry name `bedrock`.
It does not implement the Converse protocol — that is P07's
`BedrockConverseAdapter` in `heycode-llm` — and it does not resolve AWS
credentials or regions itself; PAWS01 owns both and this crate reads the
effective region from `AwsAuthService` rather than deriving a second answer.

## What survives normalization

`ListFoundationModels` publishes four facts that decide whether a model is
usable, and the shared `ModelDescriptor` vocabulary has a field for only one of
them. Discovery therefore yields `BedrockFoundationModel`, which holds the
descriptor *and* the Bedrock facts CAT02 cannot name;
`ModelCatalog::fetch` hands the registry the descriptor half.

| Bedrock fact | Retained on `BedrockFoundationModel` | Projected into `ModelDescriptor` |
|---|---|---|
| `modelLifecycle` (`ACTIVE`/`LEGACY` + four instants) | `lifecycle()` | `ModelLifecycle`: `ACTIVE`→Stable, `LEGACY`→Deprecated, `endOfLifeTime`→`retirement_at_ms` |
| `inputModalities` / `outputModalities` | `input_modalities()` / `output_modalities()`, separate lists | only `capabilities.image_input` |
| `responseStreamingSupported` | `response_streaming()`, tri-state | *(no CAT02 field)* |
| `inferenceTypesSupported` | `inference_types()` and `on_demand()` | *(no CAT02 field)* |

Output modalities, streaming support and inference types are therefore visible
through `BedrockCatalog::discover` but not through the shared registry
snapshot, which stores descriptors only. Adding fields to `ModelDescriptor` is
a `heycode-llm` (CAT02) change and is out of this row's scope.

## Conservative evidence

Unknown is never promoted. An absent `responseStreamingSupported` is
`CapabilitySupport::Unknown`; an unpublished modality or inference-type list is
`None`, which is a different fact from a published empty list; a lifecycle
phase outside the documented `ACTIVE | LEGACY` enumeration is Unknown rather
than a phase. `tools`, `reasoning`, `structured_output`, `native_web`,
`native_compaction` and `prompt_cache` have no field on this endpoint and stay
Unknown for every row. `document_input` also stays Unknown: the Bedrock
modality enumeration has no member that could express a document, and a
vocabulary that cannot say "document" is not evidence that documents are
unsupported. Only `image_input` is proven here — `IMAGE` in a published
`inputModalities` list is Supported, and a published list without it is
Unsupported.

The endpoint publishes no token limits, no aliases and no prices, so
`context_window`, `max_output_tokens`, `aliases`, `ModelPricing` and
`ModelPerformance` stay absent rather than being back-filled from
documentation.

## Strictness

Identity is strict and one malformed row rejects the whole generation: an
unsafe `modelId`, a `modelArn` outside the documented pattern, a display or
provider name that is empty, oversized or control-bearing, a duplicate id, an
unparsable lifecycle instant, a lifecycle object without its required `status`,
an unrecognized member inside a documented enumeration, an empty list or a
missing `modelSummaries` all refuse the generation. A partial Bedrock catalog
is worse than none: an absent row reads as "this model does not exist in this
region", which is a claim no failed parse has established. Unknown JSON members
are ignored, so a new AWS field cannot take a working catalog down.

The response is bounded at 2 MiB by the request cap and refused again by size
before normalization, so an oversized payload is never truncated into a shorter
model list. Status and transport diagnostics are classified without exposing
any response bytes or the credential.

## Authorization and signing

This crate performs **no request signing**. `BedrockCatalog` builds an
unauthorized request shape and hands it to a `BedrockRequestAuthorizer`, the
only place account authority is attached. The Bedrock API model advertises two
schemes, `aws.auth#sigv4` and `smithy.api#httpBearerAuth`; only the bearer
scheme is implemented here, by `BedrockApiKeyAuthorizer` over the PAWS01
credential reference `AWS_BEARER_TOKEN_BEDROCK`. A SigV4 signer belongs to
PAWS01 and plugs in as one `impl BedrockRequestAuthorizer` whose `authorize`
canonicalizes the method, path, query, headers and empty payload of the
`HttpRequest` it is given and returns it carrying `Authorization`,
`X-Amz-Date` and, for temporary credentials, `X-Amz-Security-Token`. Nothing
else in this crate changes.

Plugin `catalog-bedrock` injects `models`, `credentials`, `http` and `aws-auth`
and registers the source as a Context-owned effect. It can register before a
host region exists so the product can render Bedrock's provider-owned region
form. A normal refresh still fails safely when the effective region is
unresolved, malformed or undetermined; no region is guessed.

Draft setup accepts exactly one `region` coordinate, validates it as an
`AwsRegion`, and addresses that exact control-plane hostname. Discovery with a
masked operation key sends the key only in the bearer header. Draft results do
not publish or persist a catalog generation; model selection later stages the
region, chosen model and exact credential reference together. Fixture tests
cover the `ap-southeast-2` request independently of host environment state.

## PAWS02 — the `bedrock-mantle` endpoint

PAWS02 supplies Mantle model discovery: one `GET /v1/models` against
`https://bedrock-mantle.{region}.api.aws/v1/models`, normalized into one
all-or-nothing generation under the registry name `bedrock-mantle`.

### What "accessible" means

The list *is* the evidence. `/v1/models` answers for the credential that
authorized it and the region it was addressed to, so what it returns is what
this account can reach on this endpoint. Discovery publishes exactly the
returned rows and never unions them with a table of models AWS documents
elsewhere, because a model this account cannot call must not appear as
available. An empty list is refused rather than published: a zero-model
generation cannot be told apart from a broken response, and publishing it
would replace a good catalog with nothing.

### The ceiling on "normalized" is low, and that is the honest answer

AWS documents that on this endpoint "only `model.id` is reliable ... other
fields on `ModelInfo` may be empty". Every row therefore becomes
`ModelDescriptor::unknown`: an exact id, and Unknown for every capability,
lifecycle, limit and price. The wire type has exactly one field — there is no
`created`, `owned_by` or `object` on it to be mistaken for evidence. The id is
also the display name, because an id is the only name this endpoint honestly
supports.

Richer per-model capability evidence would mean joining PAWS03's
`ListFoundationModels` data against this list. That is a separate row, not this
one.

### Endpoint capabilities are not model capabilities

AWS publishes a rich capability list for `bedrock-mantle` — server-side tool
use, web search, asynchronous inference, prompt caching, Projects and
Workspaces. Every one of those describes the **endpoint**. Attributing them to
each model the endpoint lists would turn Unknown into Supported for models that
support none of them, which is the exact promotion this project keeps catching.

The separation is structural rather than conventional: endpoint-level facts
live in `mantle_provider_descriptor()`'s protocol list, which is a
`ProviderDescriptor`; model-level facts live in `ModelCapabilities` on a
`ModelDescriptor`. They are different types, nothing converts one into the
other, and a named test pins that the endpoint's advertised protocol support
leaves every model capability Unknown.

### No signer, and no hardcoded region list

Mantle accepts both SigV4 and a Bedrock API key, and the API key is a plain
bearer token, so this row is fully live with no signer at all — it reuses
`BedrockApiKeyAuthorizer` unchanged.

AWS publishes the list of regions where `bedrock-mantle` exists. That list is
deliberately **not** encoded here: a hardcoded allowlist would reject a region
AWS adds later, which is the failure mode every stale snapshot produces. An
unsupported region fails DNS or answers a non-success status, and the shared
classifier reports that honestly.

### Strictness

Identity is strict and one malformed row rejects the whole generation: a
missing, empty or unsafe `id`, or a duplicate id, refuses the generation rather
than being dropped from it. The response is bounded at 1 MiB, refused again by
size before normalization, and required to be JSON. Status and transport
diagnostics reuse the same classifier as the runtime catalog, so the two AWS
surfaces cannot drift apart on what a 403 or a timeout means.

Plugin `catalog-bedrock-mantle` injects `models`, `credentials`, `http` and
`aws-auth`, declares the exact contribution `model_catalog:bedrock-mantle`, and
registers the source as a Context-owned effect. As with PAWS03 there is no
built-in fallback region.

## PAWS04 — the Converse inference route

PAWS04 adds the provider-profile implementation needed to make `bedrock` an
inference route rather than a catalog with no way to run a turn. Root selection
is still unwired. It contains **no protocol code**: the Converse
request/response shapes and the AWS event-stream framing are P07's
`BedrockConverseAdapter` in `heycode-llm`, along with their conformance tests. This
row owns which endpoint, which credential, which identity, and which models may
legitimately be dispatched once composed.

`BedrockConverseProvider` advertises that adapter through
`Provider::inference_adapter()`. The legacy `Provider::stream` chat path is
**refused**, not degraded — there is no OpenAI-compatible client behind
Converse to fall back to, and P08's rule is that a provider which advertises an
adapter must fail loud rather than quietly taking another route.

`new` is the explicit fixed-secret embedding/test boundary.
`from_credentials` retains the exact `CredentialQuery` through
`RouteCredential::registry`: construction does not inspect or freeze the key,
each operation resolves once before its first attempt, retries share only that
operation's value, and rotation reaches the next operation. Missing/unreadable
credentials fail before an HTTP request exists, and provider profile discovery
reports the exact custom reference instead of substituting the default name.

`AwsInferencePluginConfig` closes provider construction without inventing a
region, model, credential or policy. `converse` requires an explicit region,
API-key query, default id, `BedrockConverseModelEvidence`, and explicit optional
runtime metadata. `mantle_responses` and `mantle_messages` require protocol-bound
model membership; Messages additionally receives the explicit optional output
cap. `aws_inference_plugin` registers the provider through an exact Context
effect and removes it on rollback/shutdown.

### Which models may be dispatched

This adapter streams, and streaming support is per model. AWS documents the
test: call `GetFoundationModel` and read `responseStreamingSupported`. PAWS03
already retains exactly that as tri-state evidence, because `ModelDescriptor`
has no field for it — so `converse_stream_eligible` is the join between the two
rows, and it answers `true` only for explicit `Supported`. A model whose
streaming support AWS never published is not a model this adapter may be
pointed at. `BedrockConverseModelEvidence::from_discovery` now performs that
join and also requires affirmative on-demand evidence; CAT02's descriptor-only
snapshot remains insufficient, so root must retain or refresh the provider row
before constructing the plugin.

For ordinary configuration, `AwsInferencePluginConfig::converse_live` removes
the need for root to construct that evidence. The effect-owned plugin registers
the provider-owned catalog and inference provider together without performing
I/O. A successful live catalog refresh populates a private complete rich
generation; request-option resolution and adapter resolution both require the
selected row's streaming and on-demand facts to be `Supported`. Missing,
`Unsupported` and `Unknown` evidence fail before the inference request exists.
The async `Provider::prepare_inference` hook forces that refresh after the
selected model/N01 routes exist and before options, P10 or durable request
admission. It returns a Converse operation snapshot holding only the verified
selected row, so a concurrent later refresh cannot change that call's evidence.
Forced failure never falls back to a stale rich generation.
The original `converse` constructor remains for a trusted host that already
owns exact provider evidence.

### No default model is invented here

`default_model` comes from the caller's own `[llm] model` configuration. AWS
publishes model and inference-profile ids on per-model pages, but does not name
one universal Bedrock default. Both candidate forms carry constraints a static
constant cannot satisfy: a geography-prefixed inference profile is
source-region/data-residency bound, and a bare foundation model id only resolves
where that model offers on-demand throughput. Converse itself accepts either
form — the requirement to name a cross-Region inference profile belongs to the
Responses API on the runtime endpoint, not to Converse.

### Cache request and response accounting

The shared adapter now accepts PAWS06's exact runtime-metadata option and places
canonical cache points after tools, system and/or the latest serialized user
turn. A requested content plane must exist before resolution succeeds. Detailed
usage preserves `cacheReadInputTokens`, `cacheWriteInputTokens` and the bounded
`cacheDetails` TTL partition without adding writes twice; absent counters remain
unknown while explicit zero remains observable. These fixtures prove wire and
normalization semantics, not a hosted cache hit.

Agent now calls `request_options_for` after exact selected-model/N01 resolution
and before P10/header commit. The provider-level fixture proves the resulting
cache points, guardrail, route option and exact detailed cache usage travel
through one successful turn. Root composition still has to select this AWS
plugin and supply an explicit Settings/config owner for the policy.

### Client tool routing

Converse accepts durable client and MCP implementation selections used by the
Agent for local tool dispatch. These selections remain in the resolved call;
ordinary tool definitions and tool results use the Converse wire format.
Provider-hosted native-tool selections still fail before dispatch because this
adapter has no configured hosted-tool dialect.

The CLI cloud protocol matrix exercises the production Converse and both Mantle
wrappers through a real Agent file read, tool-result replay, a second user turn
and portable-summary compaction. It compares every advertised tool schema and
replays persisted reasoning state exactly. HTTP responses and catalog evidence
are synthetic; this does not establish live AWS permissions, model access or
inference quality. No shipping InvokeModel adapter is covered or claimed.

### Converse reasoning state is lossless and route-bound

P07 publishes visible reasoning deltas and buffers one complete ordered
assistant message as `BedrockConverseMessage`. Text, tool-use blocks,
`ReasoningTextBlock.signature` and redacted reasoning continuity enter provider
state only after terminal success. Core/session validate the closed union,
ordinary Debug redacts its data, exact provider/model/protocol routes replay it
byte-semantically, and a mismatched state fails before transport. A neutral
assistant/tool message still cannot substitute for opaque reasoning state.

### What the fixtures prove, and what stays unobserved

Every case runs against an injected transport. The stream, tool, cache,
guardrail and reasoning fixtures drive real AWS event-stream bytes through the
real adapter from the composed profile, so they prove the provider boundary end
to end — endpoint, bearer credential, path, request body, exact detailed usage,
lossless state and replay.

They do not prove the route works against AWS. `live_converse_smoke` is written
and gated behind `HEYCODE_E2E=1` plus a real key, region, selected target and
underlying foundation-model id. Before inference it runs the provider-owned
control-plane discovery and requires affirmative `responseStreamingSupported`
for that foundation model; an operator flag cannot manufacture the evidence.
It has **never been observed green** on this host. Read the row as
fixture-verified, not live-verified.

## PAWS05 — Mantle Responses and Messages profiles

PAWS05 makes the PAWS02 catalog usable through two alternative inference
profiles under the same provider id `bedrock-mantle`:

| Profile | Base passed to shared adapter | Final request | Authentication |
|---|---|---|---|
| Responses | `https://bedrock-mantle.{region}.api.aws/v1` | `POST /v1/responses` | `Authorization: Bearer` |
| Messages | `https://bedrock-mantle.{region}.api.aws/anthropic/v1` | `POST /anthropic/v1/messages` | `x-api-key` + `anthropic-version: 2023-06-01` |

`MantleResponsesProvider` and `MantleMessagesProvider` advertise the existing
P03 and P05 adapters respectively. Their legacy Chat path refuses before
transport. `MantleInferenceProfile` carries the selected protocol beside the
shared catalog identity; two functions that merely returned identical
`ProviderProfile` values would not have represented a selection.

The current AWS model/API matrix is enforced at a conservative family
boundary. Responses refuses an Anthropic (or any non-OpenAI/non-xAI) model;
Messages refuses a non-Anthropic model before transport. A matching family is
still `Unknown`, not `Supported`: PAWS02's `/v1/models` list is exact account
evidence for Responses only, while an exact Messages-compatible model still
needs current model compatibility evidence above this crate. This avoids both
the old false fixture (one Anthropic id used through both protocols) and a
static table pretending to be the account's live catalog. An incompatible
configured default fails provider construction, so a bad profile does not wait
for the first turn to reveal itself.

Endpoint facts stay separate from model facts. `MantleProfileCapabilities`
records the documented differences: Responses supports background inference,
server-side tools and Projects; Messages has Workspaces, no background mode,
and explicitly rejects `output_config.format` structured output. Messages
server-side tools stay Unknown because its feature page documents client tool
use but no provider-side request definition. PAWS02 rows remain
`ModelDescriptor::unknown`; an endpoint capability never promotes a model.

The Messages output-token default is caller-supplied. Passing none requires an
explicit cap on every request, because PAWS02 publishes no trustworthy model
limit and this crate does not invent one.

Both providers have the same fixed-secret versus operation-time construction
split as Converse. `live_mantle_profile_smoke` runs only when
`HEYCODE_E2E_BEDROCK_MANTLE_PROTOCOL=responses|messages`,
`HEYCODE_E2E_BEDROCK_MANTLE_MODEL`, a valid resolved region and a process-scoped
Bedrock key are all present. The key enters no diagnostic or assertion.
Their small protocol guard delegates the caller's exact cancellation token and
any native-compaction surface to the shared adapter; wrapping a resolver does
not create a second lifecycle owner.

`mantle_responses_live` and `mantle_messages_live` provide the ordinary
activation seam. Their plugin owns `/v1/models` plus inference as one effect
generation and performs no composition-time request. Responses treats exact
account-visible membership as its protocol evidence because AWS defines that
listing for Responses. Messages additionally requires one exact id in the
provider-maintained current AWS Messages matrix; a merely Anthropic-shaped or
user-asserted id stays Unknown and fails before transport. The two variants
still collide on both `model_catalog:bedrock-mantle` and
`inference_provider:bedrock-mantle`, so composition can never select both.
Their preparation hook likewise forces/join-settles the account catalog before
synchronous resolve; supplied-evidence providers return immediately.

The deterministic fixtures exercise exact URLs, headers, body shapes,
settlement, and the pre-transport structured-output refusal. Neither profile is
wired into `heycode-cli`, and neither has been observed against a hosted AWS
endpoint. Root composition must select exactly one profile because both own the
same provider id.

## PAWS06 — cache, guardrail and cross-Region request metadata

The provider-owned half is implemented as
`BedrockRuntimeRequestMetadata`. It produces one validated, secret-free,
schema-v1 `ProviderRequestOption` owned by provider `bedrock`, kind
`runtime-metadata`. `BedrockConverseProvider::with_runtime_metadata` retains the
policy and `runtime_request_options(selected_model)` materializes it for the
actual selected model rather than the provider default.

The option encodes three request facts in the form the shared durable request
header now retains through the model-aware hook:

- `BedrockPromptCacheConfig` supports one unambiguous checkpoint after tools,
  system and/or the latest user message. Construction requires a
  `BedrockPromptCacheCapabilities` snapshot bound to the canonical selected
  model, with affirmative placement/count/one-hour evidence. It canonicalizes
  AWS's `tools → system → messages` processing order; a different model,
  excess/unsupported plane, Unknown/Unsupported one-hour TTL, duplicate plane,
  or one-hour checkpoint after a five-minute checkpoint fails distinctly.
  Generic `prompt_cache=Supported` remains necessary but is not mistaken for
  those model-specific facts.
- `BedrockGuardrailConfig` validates the published identifier/ARN and version
  patterns plus exact `enabled|disabled|enabled_full` trace and `sync|async`
  stream-processing enums. Its Debug output redacts the identifier.
- `BedrockRouteMetadata` records the source region and classifies the selected
  target as a foundation model, geographic/global system inference profile,
  application inference profile or other supported runtime resource. It does
  not copy the target id (already durable in the request header), and it does
  not guess destination Regions. AWS says global membership may change; exact
  destinations require a current `GetInferenceProfile` result.

### Restart-applied policy ownership

Plugin `settings-aws-bedrock` effect-registers wire-visible namespace
`aws-bedrock`. Its defaults explicitly disable prompt caching and guardrails;
there is no account/provider fallback. Route metadata has no enable switch
because it records the already-selected region/target rather than changing
provider behavior.

The cache section selects an ordered set of `tools|system|latest-user-message`
checkpoints and an exact `5m|1h` TTL for each. Settings does **not** mint the
model evidence needed to use them. `AwsBedrockSettings::runtime_metadata`
requires a separate `BedrockPromptCacheCapabilities` bound to the selected
canonical target; missing/mismatched/Unknown one-hour evidence remains
unproven, while explicit denial remains unsupported. The guardrail section
maps only validated identifier/version plus the closed trace/stream enums, and
its Debug surface never renders the identifier.

`resolve_aws_bedrock_settings` parses the committed restart snapshot.
`aws_converse_config_from_settings` and
`aws_converse_live_config_from_settings` turn that provider-owned policy into
`AwsInferencePluginConfig`, so a composition root supplies only route identity,
credential reference and evidence—not cache-point, TTL, guardrail or target
wire literals. Both disabled and configured paths retain cross-Region and
application-profile route classification.

`aws_converse_live_settings_plugin` is the production activation form. It
retains the Converse plugin's exact provider/catalog inventory, waits for the
single composed Settings service, resolves PAWS06 there and activates the
provider inside the same Context transaction. The root never preloads a second
settings snapshot, so declared inventory and applied request policy cannot
come from different file generations.

### Production integration and remaining evidence

The lower request/response bridge is implemented: `ProviderOptionContext`
carries the selected model and N01 routes, Bedrock derives one exact option,
Converse independently validates and serializes it, and detailed cache facts
cross the neutral response-metadata event. Agent now materializes the option
after exact model/N01 selection and before P10/header commit. Core/session/P07
also retain one complete `BedrockConverseMessage`, including opaque reasoning
and tool blocks, with exact-route replay and data-redacted Debug.

`heycode-cli` now composes `settings-aws-bedrock` before the selected inference
row. Ordinary Bedrock uses the Settings-backed lazy Converse plugin; Mantle
selects exactly one Responses or Messages plugin from explicit protocol config.
Each lazy plugin owns its matching catalog row, so root omits the duplicate
catalog for the selected route. Composition performs no AWS request or secret
read.

The remaining evidence is hosted: a configured prompt-cache policy still
needs authoritative selected-model placement/count/TTL facts, and cache hit,
guardrail intervention, cross-Region dispatch and hosted reasoning replay have
not been observed with an operator-supplied account. Settings is policy, never
provider evidence.

## Documented references

Every AWS wire fact is cited next to the constant or type it describes:

- <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>
- <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
- <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelLifecycle.html>
- <https://docs.aws.amazon.com/general/latest/gr/bedrock.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-messages-api.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/models-api-compatibility.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/structured-output.html>
- <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_GuardrailStreamConfiguration.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-use.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-support.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/models-get-info.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/models-endpoint-availability.html>
- <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>
- <https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-use.html>

## Verification

The focused package gate executes **145 tests**: 16 library tests and 129
integration tests. Hosted tests remain explicitly gated and make no live claim
when their operator inputs are absent.

```sh
cargo fmt -p heycode-provider-aws -- --check
cargo clippy -p heycode-provider-aws --all-targets -- -D warnings
cargo test -p heycode-provider-aws
```
