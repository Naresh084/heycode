# heycode-provider-google

Provider-owned Google API boundaries for PGCP02–PGCP07.

This crate deliberately separates three products that happen to share Google
infrastructure:

- the Gemini Developer API catalog and GenerateContent features under provider
  id `google`;
- Vertex Gemini setup/readiness and GenerateContent under provider id
  `vertex-google`;
- Google Cloud project/location/ADC health supplied by
  `heycode-authorization-gcp`; and
- Anthropic Messages served by Google Cloud under provider id
  `vertex-claude`.

It does not turn protocol compatibility into a product claim. Provider-owned
request metadata and response projection live here; shared adapters and the
composition root must explicitly consume them before a route is selectable.

## PGCP02–PGCP04: catalog, thought state, and model capabilities

`GeminiCatalog` lists models accessible to a Gemini Developer API credential,
keeps only rows that advertise `generateContent`, and normalizes only fields
the model-list response actually publishes. `thinking` is evidence for
reasoning. Tool, media, hosted-web, cache, and structured-output capabilities
remain Unknown at that catalog layer.

The shared Gemini protocol adapter owns exact `Content.parts` replay, including
thought signatures. This crate does not publish Vertex Model Garden listing as
account discovery: that listing does not supply per-project callability.

Two separate catalogs provide the smallest honest reachability boundary:

- `catalog-google-vertex` publishes exactly the current official Gemini 3.7
  Flash model-card row for provider `vertex-google`, and its draft-coordinate
  path checks a project/location with one bodyless authenticated
  `fetchPublisherModelConfig` request;
- `catalog-google-claude-vertex` publishes exactly the current official Claude
  Sonnet 5 Google Cloud row for provider `vertex-claude`.

Ordinary refresh remains credential-blind for both sources and reports account
access as Unknown. Vertex setup separately validates exact `project` and
`location` inputs, requires configured ADC plus the operation-time
`cloud-platform` OAuth-token reference, and classifies the single response
without logging its body. The source never mints tokens and never offers its
OAuth reference as a generic masked API key. `publishers.models.list` remains
unused because it is a Model Garden directory, and Claude on Google Cloud
explicitly has no Models API.

## PGCP05: Google Search grounding

`GoogleSearchRequest` owns the documented GenerateContent tool entry
`{"googleSearch":{}}` and a provider request option. `GroundingProjector`
accumulates streamed chunks and supports, rebases byte offsets onto visible
assistant text, validates public sources, and emits one durable server-tool
call/result pair plus URL citations.

Search Suggestion HTML is intentionally not durable. The projector exposes it
only transiently for a host able to satisfy Google's display requirements;
the provider-neutral event types have no field in which it can be persisted.
The crate also records that a terminal cannot currently meet that display
obligation and that session retention does not enforce Google's two-year chat
history limit.

`native-google` contributes the route-specific `google:google_search`
implementation to the logical `web_search` capability.

`ExternalGroundingRequest` separately owns Vertex's
`retrieval.externalApi` tool under provider id `vertex-google`, with explicit
Simple Search and Elasticsearch schemas. Endpoints must be credential-free
HTTPS URLs. Authentication is either no-auth or an exact Secret Manager
SecretVersion reference; this durable request type has no constructor or field
for API-key bytes. Public `retrievedContext.uri` rows use the same citation
projection, while proprietary retrieval queries/snippets and non-HTTP source
identifiers never enter the normalized durable plane. `GoogleGeminiProvider`
can now bind this option to an exact `vertex-google` route/model and drive it
through the shared parser. A Vertex `google_inference_plugin` configured with
this policy owns `native_tool:vertex-google:external_grounding` and removes it
with the same Context; an empty or Developer policy publishes no such row.

## PGCP06: code execution and context caching

`CodeExecutionRequest` owns `{"codeExecution":{}}`.
`CodeExecutionProjector` correlates the documented `executableCode` and
`codeExecutionResult` parts, retains optional provider ids without letting them
collide with normalized call ids, and emits safe success,
`execution_failed`, or `deadline_exceeded` results. Full code and output stay
in exact Gemini provider state and replay byte-semantically through the shared
adapter. Provider ids are bounded opaque strings because
Google publishes no character pattern; they never become heycode call ids. Code
longer than the neutral call-input bound is reported as omitted with its byte
length rather than truncated into plausible source.

`native-google-code-execution` contributes
`google:code_execution` to the logical `code_execution` capability.

`GeminiCacheRequest` distinguishes implicit caching, which sends no field, from
explicit use. Resource identity is product-specific:

- the Developer API accepts `cachedContents/{id}`; and
- Vertex accepts the full
  `projects/{project}/locations/{location}/cachedContents/{id}` resource.

The constructors and both provider/plugin boundaries reject a resource from
the other product before request admission. `GeminiCacheUsage` binds response
metadata to that committed mode and preserves:

- absent versus explicit-zero cache counts;
- effective prompt and cached-token counts;
- per-modality cached-token details; and
- an unknown hit when the provider reported no cache count.

It does not invent an arithmetic identity between the detail rows and the
published total because the discovery schema states none.

The strict wrapper derives Search, external-grounding and code options from the
exact selected N01 route and selected-model evidence; configured but unselected
tools never reach `tools[]`. Explicit cache resources cross top-level
`cachedContent`, and cumulative streamed usage retains read/uncached facts even
when a later usage frame omits already-reported fields. Implicit mode emits no
wire option and does not manufacture a zero write count.

## Restart-applied PGCP05/PGCP06 policy

Plugin `settings-google-inference` effect-registers wire-visible namespace
`google-inference`. Every provider-executed feature defaults disabled. The
schema keeps Developer and Vertex policy in separate objects, so a short
Developer `cachedContents/{id}` resource cannot enter Vertex and a full Vertex
resource cannot enter Developer.

The Developer section explicitly selects Google Search, code execution and
disabled/implicit/explicit caching, each against an exact model-id set. The
Vertex section separately selects external Simple Search or Elasticsearch and
disabled/implicit/explicit caching. External authentication is either no-auth
or a Google Secret Manager SecretVersion reference plus placement metadata;
there is no API-key value field. That reference remains visible as a reference
in the verified Settings projection, while Debug hides it and the endpoint.

`resolve_google_inference_settings` parses the committed restart snapshot.
`google_developer_config_from_settings`,
`google_vertex_config_from_settings` and
`google_lazy_vertex_config_from_settings` build the existing
`GoogleInferencePluginConfig` variants without provider wire literals in the
composition root. The supplied/maintained `ModelDescriptor` remains the sole
capability evidence: enabling implicit cache or Search does not change an
Unknown capability, and request preparation still refuses it as unproven.

`google_developer_settings_plugin` and
`google_lazy_vertex_settings_plugin` are the production activation forms. They
resolve that same registered snapshot during apply, retain static
provider/catalog inventory, and contribute only the configured native rows
inside the activation transaction. This avoids a second settings read and an
inventory overclaim when every feature remains disabled.

## PGCP07: Claude on Google Cloud

`ClaudeVertexProfile` is a maintained Sonnet 5 route boundary. Current Google
Cloud and Anthropic primary sources evidence:

- model id `claude-sonnet-5`;
- 1,000,000 input tokens and 128,000 output tokens;
- GA lifecycle plus image, PDF, prompt-cache, and function-call support; and
- adaptive thinking and `low|medium|high|xhigh|max` effort on Google Cloud.

`ClaudeVertexControls` is the provider-owned admission boundary for those
facts. It permits only `adaptive|disabled` thinking and the five documented
effort ids, materializes the maintained adaptive/high default explicitly, and
rejects a conflicting shared Messages body before transport. Manual
`thinking.type=enabled` is not admitted for Sonnet 5. The production wrapper
keeps the configured thinking mode fixed while a request selects among the five
real effort ids; it does not manufacture a `none` effort. Explicit sampling
fields are refused and the accepted provider default is represented by
omission, matching Sonnet 5's current request contract.

Only exact `global` is admitted today. Google also offers `us` and `eu`
multi-region endpoints, but PGCP01's `GcpLocation` does not represent those
identities; a regional value is not silently substituted.

ADC preflight remains Unknown even when configured because PGCP01 intentionally
does not mint a token. `ClaudeVertexLiveEvidence` can be constructed only by
`ClaudeVertexProfile::probe`, which resolves the exact access-token credential
at operation time and observes an authenticated Sonnet 5 stream containing both
a forced client tool call and a thinking block. The probe requires matching SSE
event names, contiguous and settled content blocks, a signed or redacted
thinking block, exact `{"ok":true}` tool input, a `tool_use` stop, and terminal
settlement; cancellation remains distinct from transport failure. Deterministic
tests exercise that boundary with an injected transport. No real Google Cloud
credential was read and no live probe was run in this change.

`ClaudeVertexProvider` is the production lower wrapper. It resolves the exact
OAuth credential per operation and configures the shared Messages adapter with
a data-driven exact-model endpoint dialect: the endpoint carries the model,
`anthropic_version=vertex-2023-10-16` is in the body, auth is bearer, and no
`anthropic-version` header is sent. Tool/thinking blocks, opaque signatures and
provider state use the existing Messages parser and replay boundary rather than
a second Vertex-specific parser. Its Provider descriptor returns the maintained
Sonnet 5 model facts only for that exact id; every other id remains Unknown.
`GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE` owns the default non-secret reference
root composition should bind; PGCP01 still never reads or mints its value.

## Effect-owned inference construction

`GoogleInferencePluginConfig` has five exact constructors and no implicit
product/default selection:

- `developer(api_key_query, model_evidence, policy)` registers provider
  `google`; policy may explicitly add Search, code execution and cache;
- `vertex(base_url, oauth_query, model_evidence, policy)` registers
  `vertex-google`; policy may explicitly add external grounding and cache; and
- `claude_vertex(profile, controls)` registers exact-model `vertex-claude` with
  the supplied thinking/effort policy;
- `lazy_vertex(profile_request, oauth_query, policy)` owns the maintained
  `vertex-google` catalog and prepares the concrete endpoint on first use; and
- `lazy_claude_vertex(profile_request, oauth_query, controls)` does the same for
  the maintained `vertex-claude` route.

`GoogleGeminiModelEvidence` requires a non-empty unique descriptor set and an
included default. Every feature model set must be a subset. Cross-product
policy is refused: Developer cannot mount external grounding and the current
Vertex product does not claim Developer Search/code candidates. Catalog,
provider and candidate registrations are Context effects; native candidates
unwind before their provider, and a lazy provider unwinds before its catalog.
The older standalone Search/code candidate plugins remain available for
compositions that do not use the product plugin, but root must not register both
owners for the same row.

## Product integration status and remaining routes

The exact maintained Gemini Developer default is now a selectable production
route. Root publishes selection/registry/interception owners without claiming
the provider, composes `settings-google-inference`, then the provider-owned
Settings wrapper registers strict GenerateContent plus only configured N01
candidates with operation-time credentials. Agent invokes
`request_options_for` only after model/N01 selection and before P10/durable
commit. Real composition proves exact ownership and disposal without network
traffic; a user allowlist does not promote the Developer catalog's Unknown
model capability evidence.

### Lazy Vertex activation

`GoogleInferencePluginConfig::lazy_vertex` and `lazy_claude_vertex` use the
shared caller-cancellable Provider preparation hook. Their composition effects
are inert: the plugin registers its catalog first, then one provider whose exact
descriptor/model and inert adapter identity let Agent enter strict dispatch.
Registration performs no profile, credential, or HTTP operation. The Gemini
catalog's separate setup probe is invoked only for an explicit coordinate
draft; ordinary refresh stays maintained and credential-blind.

After catalog/model/N01 selection, the first operation resolves the supplied
explicit `GcpProfileRequest` through the composed `GcpAuthService`. The project
and location form the concrete Vertex endpoint; a full catalog descriptor
mismatch or unconfigured provider-native route fails before that resolution.
The hook returns a concrete `GoogleGeminiProvider` or `ClaudeVertexProvider`,
and ordinary provider options, P10, C02/C05, and streaming then consume that
exact operation provider. The concrete adapter retains a registry-backed
`RouteCredential`, so access-token resolution remains independently per
operation rather than being folded into profile discovery.

Project and location are mandatory constructor inputs; process/gcloud defaults
are never inherited invisibly. The Google provider never mints an OAuth token
or reads credential bytes. On shutdown, native candidates withdraw first, the
provider withdraws next, and the catalog withdraws last by Context LIFO.
Maintained catalog access remains Unknown even after profile resolution; only a
successful authenticated model operation is account evidence.

Remaining product work is **credentials and hosted evidence**: supply
operation-time access tokens
   for `CLAUDE_VERTEX_OAUTH_SCOPE`; PGCP01 health alone intentionally does not
   mint one. Secret-backed external APIs additionally require the Vertex AI
   Extension Service Agent to hold `secretmanager.versions.access`; heycode never
   reads that value. Keep diagnostics credential-blind and run `probe`/Gemini
   live smokes only under explicit operator gates. Search requires
   `HEYCODE_E2E=1`, `HEYCODE_E2E_GEMINI_SEARCH=1`, an exact model and a key; Claude
   additionally requires `HEYCODE_E2E_CLAUDE_VERTEX=1`, project and access-token
   inputs. No hosted Search, external grounding, code execution, cache use or
   Claude turn was observed here.

No hosted Search, external grounding, code execution, cache use or Claude turn
is claimed by the baseline Developer composition.

## Primary sources

- Gemini v1beta discovery document:
  <https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta>
- Gemini code execution:
  <https://ai.google.dev/gemini-api/docs/code-execution>
- Gemini context caching:
  <https://ai.google.dev/gemini-api/docs/caching>
- Vertex context-cache resource/use shape:
  <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/context-cache/context-cache-use>
- Vertex Gemini 3.7 Flash model card:
  <https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-7-flash>
- Vertex Model Garden list semantics:
  <https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/v1beta1/publishers.models/list>
- Gemini token and cache usage:
  <https://ai.google.dev/gemini-api/docs/generate-content/tokens>
- Google Search grounding:
  <https://ai.google.dev/gemini-api/docs/google-search>
- Vertex grounding with a caller-owned search API:
  <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/grounding/grounding-with-your-search-api>
- Vertex v1 ExternalApi/AuthConfig schemas:
  <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/reference/rpc/google.cloud.aiplatform.v1#externalapi>
- Claude on Google Cloud wire differences and feature support:
  <https://platform.claude.com/docs/en/build-with-claude/claude-on-vertex-ai>
- Official Anthropic TypeScript Vertex request rewrite:
  <https://github.com/anthropics/anthropic-sdk-typescript/blob/main/packages/vertex-sdk/src/client.ts>
- Claude Sonnet 5 Google Cloud model card:
  <https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/partner-models/claude/sonnet-5>
- Claude Sonnet 5 request changes:
  <https://platform.claude.com/docs/en/models/sonnet-5/whats-new-sonnet-5>
- Anthropic effort and thinking:
  <https://platform.claude.com/docs/en/build-with-claude/effort>

## Verification

The current deterministic package lane executes **141 tests**: 4 library tests
and 137 integration tests. Hosted tests are double-gated and make no live claim
when their explicit operator inputs are absent.

```sh
cargo fmt -p heycode-provider-google -- --check
cargo clippy -p heycode-provider-google --all-targets -- -D warnings
cargo test -p heycode-provider-google
```

The onboarding connection profile admits only `GOOGLE_GEMINI_3_7_FLASH`, matching production composition. Other discovered Gemini models remain catalog information until their adapter activation is supported.
