# heycode-provider-openrouter

Provider-owned OpenRouter product contributions.

POR01 moves `openrouter-api-key` out of the shared API-key compatibility
plugin, preserves exact credential queries, exposes the safe provider profile
and registers the flow as a Context-owned effect. Schema v12 preserves that
flow for historical exact profiles.

POR02 adds effect-owned public catalog source `catalog-openrouter`. It validates
the complete `/models` generation, cross-checks singular
`/model/z-ai/glm-5.3-flash`, and publishes conservative limits and explicit
capability evidence. The safe live canary requires no credential.
The deterministic list/detail fixtures now cross CAT08's strict schema-v1
envelope and carry the exact official endpoint, API-v1 source label and
non-zero capture instant before their payload reaches the catalog normalizer.
`architecture.input_modalities` now maps `image` and `file` independently;
absence is explicit Unsupported evidence. The current GLM row advertises
image/video but no file modality, so ATT03 chooses bounded local extraction
instead of mislabeling a gateway PDF request as native.

The current provider default is `z-ai/glm-5.3-flash`. Strict inference now uses
durable routing policy, max/high/low effort with default max, and complete
reasoning/reasoning-details preservation across tool turns. Schema v13 keeps
older exact OpenRouter profiles loadable. POR04 remains active only for a
trustworthy authenticated live tool-turn artifact.

Catalog admission now binds that strict adapter contract to the selected GLM
row. The generation is rejected if mandatory/default-enabled reasoning,
ordered max/high/low efforts with default max, or the exact
`reasoning`/`tools`/`tool_choice` parameters drift. A generic object-shaped
`reasoning` field can no longer authorize a hard-coded effort vocabulary.

POR05 adds plugin `native-openrouter`, which contributes exact candidate
`openrouter:web_search` as an effect. The strict Chat route sends the current
server-tool `parameters` object with heycode-bounded auto/5-results/3-uses/
15-total/4,000-character policy and top-level five-call budget. Provider
selection removes the duplicate client search; catalog rows advertise the
gateway's documented any-model fallback. When configured Chat events carry
`url_citation` annotations, they are validated, preserved and committed through
N02; their exact streaming location remains Unknown as described below. Schema
v15 restores the candidate for historical exact OpenRouter profiles.

The official Chat surface exposes final `url_citation` annotations and aggregate
`web_search_requests`, not exact per-search ids/queries/results. POR05 therefore
remains active pending trustworthy authenticated raw evidence or a Responses
integration; the implementation never manufactures synthetic calls. The
provider-local two-request fixture proves the server-tool definition and budget
reach wire, citation normalization, exact annotation replay through provider
state, and aggregate usage without an invented call/result event. Its
`delta.annotations` placement is explicitly `Synthetic`: the current generated
`ChatStreamDelta` schema does not document annotations, so the fixture is not a
claim that OpenRouter emitted those bytes.

The protocol evidence was rechecked against OpenRouter's current
[web-search server-tool guide](https://openrouter.ai/docs/guides/features/server-tools/web-search).
It documents `max_uses` as the per-tool search cap, `max_total_results` as the
cumulative result cap, and top-level `max_tool_calls` as the shared server-tool
step budget. The guide shows aggregate
`usage.server_tool_use.web_search_requests`, while the current generated Chat
schema uses `usage.server_tool_use_details.web_search_requests`; these are
documented aliases only when equal, and a conflict must fail rather than pick a
winner. Neither surface defines a Chat call id, query item or result block.
The Responses schema separately defines `web_search_call` and
`openrouter:web_search` output-item families, but the guide does not promise
which one is emitted and some provider-specific identity/action fields are
optional. That different protocol is a future evidence route, not authority to
fabricate items on this Chat route.

The current [streaming reference](https://openrouter.ai/docs/api/reference/streaming)
also documents a final usage chunk that repeats the same `finish_reason` as the
terminal content chunk. The shared Chat parser accepts that exact content-free
usage-frame repeat while rejecting a changed reason or late output. A synthetic
provider fixture still cannot substitute for authenticated wire evidence.

A provider-local live probe now runs only with both `HEYCODE_E2E=1` and a
process-scoped `OPENROUTER_API_KEY`. Its transport observer retains only closed
citation counts, aggregate counts and duplicate flags; neither the credential
nor raw response content enters diagnostics. It recognizes both documented
aggregate aliases only when they agree. A passing probe requires a
citation, a positive aggregate and terminal settlement while still emitting no
synthetic normalized call/result. It is evidence infrastructure, not the
freshness-gated raw artifact or durable root bridge needed to accept POR05.

POR07 extends the scheduled lane without duplicating the production Agent
turns. The existing production-composition test first writes a fresh,
content-withheld catalog/reasoning/text/exactly-once-tool artifact. The
provider-local follow-on accepts only that exact passed prerequisite, then sends
one real `openrouter:web_search` request with explicit fallback, parameter and
data-collection routing fields plus all-disabled transform policy. Its transport
observer retains only booleans, counts and conflict flags; the adapter must
normalize a citation and positive aggregate usage into replay-ready Chat state
without inventing a call/result id. Only then is a second seven-day-fresh
content-withheld artifact written with the closed claim that catalog, text,
tool, search and routing canaries passed. Missing credentials or a missing,
failed, stale or future-dated prerequisite produce a skipped artifact and fail
the lane. No authenticated pass exists in this checkout, so POR04/POR05/POR07
remain external evidence rather than completed support claims.

POR06 adds `OpenRouterTransformPolicy`, an explicit decision for OpenRouter's
request plugins. Two implicit-enable paths exist: OpenRouter defaults context
compression on for endpoints of 8k context or less, and an account-level
default can enable every plugin a request does not mention. Sending no
`plugins` field is therefore not "off" — it is "OpenRouter and the account
decide". The policy always serializes one entry per known transform, using the
documented `"enabled": false` form for every transform heycode did not ask for,
and the documented bare-id form for one it did.

`activations()` records the decision for every transform and `enabled_activations()`
for the ones heycode switched on, each carrying the transform's effect and its
cost. Costs stay typed: mistral-ocr is an exact $2 per 1,000 pages in
pico-units, cloudflare-ai and response healing are documented free, native
parsing bills as upstream input tokens, and an unpinned document engine is
Unknown — OpenRouter picks per model, so an unpinned fee must never render as
zero. Context compression publishes no fee at all, which is Unknown, not free.

`effective()` is always `Unknown` by construction. An account can set "Prevent
overrides" so a request cannot change a plugin's configuration, so the request
proves what heycode asked for and never what ran. Promoting it would report a
guess as evidence.

Provider-option construction is request-aware. Response healing is refused for
a streaming request or one without structured output, matching OpenRouter's
documented non-streaming `json_schema`/`json_object` boundary. The current heycode
strict Chat route always streams, so product integration must keep response
healing explicitly disabled until a compatible request surface exists.

OpenRouter's `web` plugin is deliberately outside this policy: the deprecated
plugin is superseded by the `openrouter:web_search` server tool that POR05
already owns, and one feature reaching the wire through two mechanisms would
let each hide the other.

`OpenRouterTransformPolicy::provider_option` now crosses the shared/product
boundary. The Chat adapter accepts multiple unique option dialects, maps routing
as a whole object and unwraps the transform option's sole `plugins` member.
Every `OpenRouterProvider` constructor requires that option; the CLI production
factory supplies `all_disabled()`. C02/C05 persist and verify routing plus
transforms, and the mock production turn asserts the exact top-level wire array.

N05 now exposes the same policy through provider plugin
`request-transforms-openrouter`. It effect-registers three exact
`request_transform` rows into the shared registry, mapping requested/effective,
effect and cost without duplicating OpenRouter wire knowledge. The shared P10
layer verifies the already-required provider option or inserts it for another
compatible Consumer; a conflict is refused rather than overwritten. Context
shutdown removes all three rows. The deprecated `web` plugin remains outside
this registry because POR05 owns the replacement server tool.

N06 preserves either documented aggregate spelling as `ServerToolUsage` with
logical `web_search`, `ProviderAggregate` evidence and Unknown cost under the
current `auto` engine. Equal aliases coalesce, conflicts fail, and generic
server-tool details without a web count remain non-web evidence. The aggregate
stays distinct from exact call/result events, so the Synthetic provider fixture
still proves the parser invents no query or call id.

## Focused verification

```sh
cargo fmt -p heycode-provider-openrouter -- --check
cargo clippy -p heycode-provider-openrouter --all-targets -- -D warnings
cargo test -p heycode-provider-openrouter
```
