# heycode-provider-anthropic

Provider-owned Anthropic product contributions.

PAN01 supplies the auth/catalog/profile plugins around the reusable
`AnthropicMessagesAdapter` that P05 already implements. This crate never
re-implements the Messages protocol; it owns the registry name `anthropic`, the
`AnthropicMessages` descriptor, the provider default `claude-opus-5` and the
non-secret credential reference `ANTHROPIC_API_KEY`.

Plugin `provider-anthropic` owns `authorization_flow:anthropic-api-key` and
registers it as a Context-owned effect. `AnthropicApiKeyValidator` sends the
native `x-api-key` header plus the required `anthropic-version: 2023-06-01` —
there is no bearer dialect here, so the shared `HttpApiKeyValidator` cannot be
reused. Key acceptance and model entitlement stay separate proofs: `GET
/v1/models?limit=1` shows the key is accepted, and only `GET
/v1/models/{model}` shows the configured id exists for it, so a `404` on the
second request is `model`, never `unauthorized`.

Plugin `catalog-anthropic` adds the effect-owned authenticated catalog source.
It resolves its credential at refresh time, follows the documented `has_more` /
`last_id` cursor with `limit=1000` under a sixteen-page budget, and publishes
one all-or-nothing generation. Identity and limits are strict — a non-`model`
row, an unsafe id, a non-RFC-3339 `created_at`, a zero token limit, a duplicate
id across pages or a cursor that disagrees with the returned rows rejects the
whole generation. Status and transport diagnostics are classified without
exposing any response bytes.

Capability normalization stays conservative. `thinking`, `image_input`,
`pdf_input`, `structured_outputs` and `context_management.compact_20260112` are
exact row evidence; a missing capability field is Unknown rather than a
rejected generation. `tools` and `native_web` have no equivalent field or
provider-wide guarantee and stay Unknown. Prompt caching is different: the
current primary guide explicitly supports automatic and explicit caching on
all active Claude models, so every successfully validated account-scoped list
row receives `prompt_cache=Supported`. Lifecycle is Unknown because the
endpoint publishes no deprecation or retirement evidence, and `aliases` is
empty because the list publishes no alias field (`/v1/models/{alias}` resolves
one alias at a time and is not a generation source).

Anthropic publishes prices in documentation, not through any API endpoint, so
every row records `ModelPricing::unknown()` and `ModelPerformance::unknown()`
rather than transcribing a docs table as if it were API evidence.

Plugin `token-count-anthropic` measures representable transcript messages with
the provider's `POST /v1/messages/count_tokens` endpoint. It now preserves the
current documented tool-result/tool-call/image/PDF shapes and refuses an empty
transcript or malformed structure rather than dropping blocks. C11 measures
system, tool-schema and lossless provider-state contributors separately; the
endpoint now declares the shared `ProviderTokenizer` estimate class rather than
claiming Exact or falling to the unrelated local byte ratio.

PAN01 is complete against its profile/catalog/token-count acceptance and
deterministic transport fixtures. No Anthropic credential exists on the build
host, so `live_official_catalog_publishes_the_default_model_when_enabled` skips
unless `HEYCODE_E2E=1` and `ANTHROPIC_API_KEY` are both set. That disclosed live
evidence belongs to the provider canary matrix; it is not fabricated here.

## PAN02 — thinking and interleaved tool continuation

`AnthropicProvider` wraps the reusable Messages adapter with the provider-owned
thinking contract. The default `claude-opus-5` route uses adaptive thinking and
the documented `low | medium | high | xhigh | max` effort values; `none`
disables thinking without also sending an effort. Adaptive thinking interleaves
around tool results automatically and sends no beta header.

Production construction binds the configured non-secret credential reference
through `RouteCredential`; it does not retain the value resolved during startup
preflight. Each Messages operation acquires that exact route once, retries reuse
the operation value, and the next operation observes rotation without
recomposition. The strict authentication preview records the configured handle
rather than `AdapterOwned`.

Ordinary turns supply the Messages API's required output-token limit when the
caller omits it: 8192 tokens, capped by the selected model's published limit.
Portable summary requests use ordinary Messages inference. Only requests that
explicitly select `NativeFeature::Compaction` enter the native compaction path;
the shared compaction purpose alone does not imply native feature support.

The separate manual constructor requires an explicit model and rejects the
known adaptive-only provider default. It sends
`interleaved-thinking-2025-05-14` plus a validated token budget. This prevents
the previous silent fallback that would receive a live 400 from Opus 5; because
the Models API publishes no thinking-dialect inventory, the caller still owns
compatibility for every other explicit id.

Continuation is checked after shared request resolution and before transport:

- a neutral assistant tool call is refused because it cannot carry the original
  thinking blocks;
- every replayed `thinking.signature` and `redacted_thinking.data` must remain
  non-empty;
- each step of an interleaved tool chain is checked independently;
- manual thinking additionally requires the continued assistant turn to begin
  with `thinking` or `redacted_thinking`;
- the requirement ends when a standard user message opens a new turn, matching
  Anthropic's rule that prior thinking may be omitted outside tool use.

The end-to-end stream test replays the exact `AnthropicMessage` provider item
published by the parser and asserts the opaque signature reaches the next HTTP
request byte-for-byte and in its original block order. heycode never interprets or
re-signs it.

Official sources:

- <https://platform.claude.com/docs/en/about-claude/models/extended-thinking-models>
- <https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking>
- <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
- <https://platform.claude.com/docs/en/api/cli/messages>

Known limits: heycode checks opaque-field presence, not Anthropic's cryptographic
signature; the service remains the authenticity authority. Model capability is
still route-level because the Models API exposes thinking support but no
dialect/effort inventory. The manual constructor rejects the known incompatible
default but cannot prove an arbitrary explicit model supports manual
interleaving. No CLI/provider-registry activation is added here; that shared
product wiring remains a separate owner decision.

## PAN03 — exact server-tool definitions and pause-state facts

`AnthropicServerToolKind` covers web search, web fetch, code execution, advisor,
regex tool search and the MCP connector. Definitions pin current active version
strings and required names. Basic search/fetch carry an explicit provider-owned
five-use bound. Advisor and MCP retain their exact beta values; MCP also carries
the credential-free top-level `mcp_servers` extension plus matching
`mcp_toolset`. Literal authorization is deliberately unrepresentable.

Capability evidence is per model/tool pair and never inferred from Messages
compatibility. The current code-execution and tool-search tables are transcribed
exactly, including Opus 5; advisor admission validates the executor/advisor pair
rather than either model alone. The Opus 5 migration guide explicitly makes web
fetch `Unsupported`. Unknown ids and undocumented pairs stay `Unknown`.

`AnthropicServerToolPlan` is the provider-owned gate and durable option. It
builds the generic `heycode-llm` Messages plan with exact request tools, beta
values, top-level fields and call/result routes. Code execution maps the real
`bash_code_execution` and `text_editor_code_execution` subcalls rather than an
invented generic call. Multiple MCP toolsets share one server-name-restricted
route. Tool search refuses activation without exact companion names, and the
generic serializer proves those offered definitions receive
`defer_loading:true`.

Provider-state classification handles zero-to-many calls, actual ids, paired
success/error results, public web sources and URL citations without exposing
input/output/opaque citation material in Debug. It preserves the complete
`AnthropicMessage` unchanged. MCP honors top-level `is_error`; code execution
validates its subcall-specific result type. Orphan, duplicate, cross-family and
unadvertised blocks fail closed.

A normalized `pause_turn` yields `AnthropicPendingPauseState`: the whole exact
assistant item plus its actual unresolved ids and identical required provider
option. Mixed client/server turns remain `tool_use` and wait for client results
instead. The production provider wrapper now registers the generic parser plan,
serializes it and accepts its normalized call/result events. A two-request
fixture crosses pending call → `FinishReason::Pause` → exact state replay →
later result settlement. Completed advisor history may remove the live tool
while retaining its beta; a pending call cannot remove the selection.

Server-tool intent is no longer a static provider option. Every family exposes
an exact N01 implementation id `anthropic:<logical>`, and
`Provider::request_options_for` emits `server-tools` only for matching
provider-owned routes. The current shared Messages plan is atomic, so a partial
route set fails instead of enabling unselected definitions; zero provider
routes selects no server tool and still preserves historical parser routes and
required beta values. Web routes arrive from Agent with the generic `Web`
feature, but provider resolution consumes that marker only after the exact
per-model `server_tool_support` gate, because the authenticated Models endpoint
has no native-web field from which the common resolver could derive evidence.
The durable N01 route and exact provider option remain in the resolved call.

`AnthropicMaintainedDefaultServerToolPolicy::atomic_zero_configuration()` is
the explicit product constructor and deliberately has no `Default`
implementation. It admits one atomic plan—web search plus code execution—
against the exact maintained `claude-opus-5` evidence before publication. Root
can configure a Provider with one call to
`configure_anthropic_default_server_tools(provider)` and register the identical
candidate set through the effect-owned
`anthropic_default_native_tools_plugin()`. Web fetch is excluded because the
maintained default explicitly does not support it; advisor, deferred tool
search and MCP remain excluded because they require additional model/tool/server
configuration.

The provider-owned `anthropic-server-tools` Settings namespace now supplies
that missing configuration while preserving the same named atomic baseline.
It is restart-applied and wire-verified, with no credential value slot:

- advisor uses explicit `disabled|enabled`, a required advisor model when
  enabled, and a positive per-request `max_uses` visible even while disabled;
- regex tool search uses explicit `disabled|enabled` and, when enabled, one to
  sixty-four unique current tool names matching the documented 64-character
  identifier grammar. Those exact names become `defer_loading:true` through
  the shared Messages plan;
- MCP uses explicit `disabled|enabled` plus up to twenty unique credential-free
  HTTPS `{name,url}` records. The Settings shape cannot carry
  `authorization_token`, headers, or OAuth material.

`AnthropicConfiguredServerToolPolicy::resolve(settings, executor_model)` builds
one exact Search/code plus optional advisor/tool-search/MCP plan and admits
every definition/pair against current tri-state evidence before publication.
It never adds web fetch: `claude-opus-5` has affirmative Unsupported evidence.

The maintained-default baseline is admitted per family against the executor, so
Unknown is absent rather than fatal: an executor keeps only the baseline
families its own evidence proves, and an executor that proves none yields a
valid policy with no plan (`plan()` is `None`), no N01 candidate and an
unconfigured provider, so the plugin still activates. Explicitly configured
advisor, tool-search and MCP families are never downgraded that way — they name
what the operator asked for, so Unsupported or Unknown evidence for the
executor or the advisor pair refuses the value outright.
The matching
`anthropic_configured_native_tools_plugin(executor_model)` retains the existing
`native-anthropic` id, registers the namespace, derives the identical plan, and
effect-registers one N01 row per logical family (multiple MCP servers still
share `remote_mcp`). Invalid, partial, duplicate, unsafe, credential-shaped,
Unsupported, or Unknown configuration publishes nothing; shutdown removes the
rows and namespace cleanly.

The composition root must replace—never compose beside—the former
`anthropic_default_native_tools_plugin()` factory with
`anthropic_configured_native_tools_plugin(selected_model)`, then apply exactly:

```rust,ignore
let tools = AnthropicConfiguredServerToolPolicy::resolve(&settings, selected_model)?;
let provider = tools.configure(provider)?;
```

Root must not reconstruct tool definitions, beta values, deferred names, MCP
metadata, or N01 identities.

Citation normalization now matches the current API schema's optional nullable
title: `title:null` becomes `UrlCitation { title: None }` while the complete
provider block, including its opaque `encrypted_index`, remains byte-semantic
replay state. Non-string or empty titles still fail closed.

Official sources:

- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-reference>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/server-tools>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool>
- <https://platform.claude.com/docs/en/agents-and-tools/mcp-connector>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/advisor-tool>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool>
- <https://platform.claude.com/docs/en/about-claude/models/migration-guide>

The remaining root bridge must select the settings-backed factory/resolver and
prove the route through production composition and the already-shipped generic
N02 Agent/session/UI durable path. The current provider plan is atomic: a
partial provider-native route selection fails loudly rather than enabling an
unselected family. No live authenticated server-tool call is claimed. Existing
live tests remain explicitly gated by `HEYCODE_E2E=1` and never put credentials or
provider bodies in diagnostics.

## PAN04 — native server compaction

`AnthropicCompactionDefinition` owns the exact `compact_20260112` edit, the
`compact-2026-01-12` beta value, the documented 50,000-token trigger floor and
optional pause behavior. Capability admission follows the current exhaustive
supported-model table, which now includes `claude-opus-5`, Opus 4.6–4.8,
Sonnet 4.6/5, Fable 5 and Mythos 5/Preview. Other unlisted ids remain Unknown.

`AnthropicCompactionCheckpoint` admits one complete assistant provider item
containing exactly one non-empty compaction block. It records only the block's
index; the block, adjacent thinking/text and future extension fields remain in
the unchanged `ProviderStateItem`. The provider fixture serializes/restores the
whole item and proves the production Messages serializer sends it byte-exact on
continuation. Diagnostics expose neither summary content nor signatures.

`AnthropicProvider` now advertises C12's optional
`InferenceAdapter::native_compaction` operation. A compaction-purpose call must
carry `NativeFeature::Compaction` and exact model capability evidence. The
provider inserts the definition and beta into the one no-retry Messages
operation, normalizes Anthropic's `compaction` stop into the existing Pause
invariant, waits for the complete terminal stream, validates exactly one
non-empty block and returns a shared `NativeCompactionCheckpoint`. The Agent
remains the only durable commit owner. Cancellation settles before return and
cannot produce a checkpoint.

The bridge preserves the complete parser-published assistant item: adjacent
thinking/signatures, text/tool blocks, the compaction block and unknown block
extensions are never distilled. The fixture crosses the real C12 trait,
asserts the exact beta/body, and reuses the checkpoint state byte-for-byte on a
later Messages request.

The turn *after* a compaction is the one that would silently lose this
property. It carries no native feature and no provider route, so the shared
resolver — which proves replay safety from those two facts alone — would hand
it the standard replayable policy, and an ambiguous transport failure would
re-send a request the provider had already partly committed. The route
therefore keeps whichever of two proofs is narrower: the one resolution
derived, and the one the shared Messages adapter derived from the protocol
evidence it alone reads (a `compaction` or `server_tool_use` block in provider
state, a container, a non-direct `tool_use` caller, a selected server-tool
plan). Both come from the one `AnthropicMessagesConfig`, so only the
replay-safety proof differs and the attempt/backoff bounds stay put. A bare
conversation turn keeps the replayable policy.

Official source:

- <https://platform.claude.com/docs/en/build-with-claude/compaction>

Production credential-backed composition now registers `AnthropicProvider` and
C12 exposes its native operation while portable compaction remains the product
default. Isolated real-composition proof verifies the exact operation-time
binding and performs no request; provider fixtures remain the beta/body/state
wire evidence. Human strategy selection remains CMD07.

## PAN05 — server-side context editing

`AnthropicContextEditingPolicy` owns both current beta strategies and makes
their required order structural: `clear_thinking_20251015` is always emitted
before `clear_tool_uses_20250919`. Thinking retention is explicit (`all` or a
positive turn count). Tool clearing retains both documented trigger dialects
(input tokens or tool uses), recent-use count, minimum useful clearing,
exclusions and optional tool-input clearing; excluded names are bounded and
omitted from diagnostics.

`AnthropicContextEditReport` parses the terminal
`context_management.applied_edits` array into separate thinking/tool facts,
preserves each complete metadata object for durable recording, checks the
cleared-token sum and records cache impact as `Preserved` or
`InvalidatedAtEdit`. Cleared input tokens never become ordinary input usage.
The provider-owned boundary does not rewrite conversation history: Anthropic
applies edits server-side and explicitly instructs clients to retain their full
unmodified local history.

Official source:

- <https://platform.claude.com/docs/en/build-with-claude/context-editing>

`AnthropicProvider::with_context_editing` now makes that lower policy a
production request path. Its exact secret-free JSON is committed as provider
option `context-editing`; resolution requires maintained model evidence; the
provider-local transport merges the ordered edit list plus beta without
teaching the reusable Messages adapter Anthropic product policy. The response
observer validates the final streaming `context_management.applied_edits`
before Finish and publishes a bounded, response-id-correlated
`AnthropicResponseMetadata` record only after successful terminal settlement.
Malformed or failed streams publish no record.

`AnthropicTokenCounterConfig::with_context_editing` applies the same edit plan
to `/count_tokens`. `AnthropicTokenCountReport` keeps the effective count and
the provider's `original_input_tokens` separately, so pressure can use the
post-edit value while a caller can explain the exact reduction.

The provider emits a neutral `ResponseMetadata` immediately before
normalized Usage/Finish. Agent commits it as correlated v2
`assistant/response-metadata`; restart projection and `/context`/`/usage`
preserve applied edit counts, cleared tokens and cache-prefix impact. The
provider-local response-id ledger remains a compatibility inspection surface,
not the durable authority. Local history is still retained in full, as
Anthropic requires.

The restart-applied `anthropic` Settings namespace now owns the opt-in policy.
Its defaults disable both context edits. Explicit modes cover keep-all versus a
positive thinking-turn count, input-token versus tool-use triggers, recent tool
uses, optional minimum clearing, exclusions and tool-input clearing. Every
numeric control has a separate visible mode and a positive value; zero is never
an omission sentinel. `AnthropicSettingsPolicies` maps one resolved generation
to both `AnthropicProvider` and `AnthropicTokenCounterConfig`, preventing count
pressure from using a different edit plan than inference.

## PAN06 — prompt caching and token-count visibility

`AnthropicPromptCachePolicy` represents current top-level automatic caching
with explicit 5-minute or 1-hour TTL. The policy is committed as provider option
`prompt-cache`; resolution materializes `NativeFeature::PromptCache`, which
requires `prompt_cache=Supported`, and the exact `cache_control` object reaches
the Messages body. No beta header is invented.

`AnthropicCacheUsage` preserves uncached input, cache creation, cache read,
optional 5-minute/1-hour write breakdown, output and the checked total input.
It classifies read/write activity and is cross-checked against the normalized
shared `TokenUsage` before terminal publication. This fixes the common error of
treating `usage.input_tokens` as the total when cache fields are nonzero.

The token counter now serializes every structured input its current official
endpoint accepts: client tools, image/PDF blocks, assistant tool calls and
coalesced tool results with durable `is_error`. Unsupported content is still
refused rather than flattened. Context-editing counts additionally expose
original versus effective input tokens.

Official sources:

- <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
- <https://platform.claude.com/docs/en/build-with-claude/token-counting>
- <https://platform.claude.com/docs/en/api/messages/count_tokens>

`ProviderTokenizer` represents the documented estimate truthfully and
production composition constructs this provider. Detailed cache/edit facts now
survive the neutral durable/usage UI boundary, including exact 5m/1h write
breakdown and cache-aware Unknown cost rules.

The same restart-applied namespace exposes `disabled`, `automatic-5m` and
`automatic-1h`; disabled is the schema default. The schema is verified for wire
exposure and registered as a `provider-anthropic` effect. The token-counter
plugin resolves the registered generation itself, while root applies
`AnthropicSettingsPolicies::resolve(&settings)?.apply_provider(provider)` to the
new strict provider. No live credentialed call is claimed.

Current deterministic inventory: **87 tests** (2 unit + 85 integration).
Credential-gated catalog/server-tool live evidence remains outside this count
unless the explicit E2E gate and a trustworthy Anthropic credential are
present.
