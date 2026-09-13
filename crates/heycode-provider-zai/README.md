# heycode-provider-zai

Provider-owned Z.ai plan profiles (PZA01), maintained GLM catalog (PZA02)
and thinking/function-call continuation route (PZA03), native web-search
metadata substrate (PZA04), and Coding Plan MCP bundle substrate (PZA05).

Z.ai sells two products behind one vendor, and this crate's whole job is to
keep them apart. The **general** (pay-as-you-go) platform and the **GLM Coding
Plan** subscription publish different base URLs, are entitled separately, and
Z.ai states plainly that a Coding Plan key "is not interchangeable with other
Z.AI's API Keys". Nothing here fabricates evidence it did not verify.

## Distinct

The plan is part of the *type*, not a field a caller can get wrong.
`ZaiEndpoint<P>` and `ZaiCredential<P>` are each bound to one plan marker
(`General` or `Coding`, sealed against outside implementations), so
`ZaiProfile::new` accepts only a matching pair. Handing a `ZaiCredential<General>`
to a `ZaiEndpoint<Coding>` does not compile; a `compile_fail` doctest pins that.

Distinctness is enforced on three axes:

- **Endpoint** — each plan answers only for the base URLs Z.ai documents *for
  that plan*. The general plan has one (`.../api/paas/v4`); asking it for the
  Anthropic Messages or OpenAI Responses URL returns `UndocumentedEndpoint`
  rather than borrowing the Coding Plan's, because those URLs would spend a
  different entitlement. There is deliberately no base-URL override.
- **Credential** — the two plans default to different non-secret references,
  and a plan refuses to be configured with the *other* plan's default even
  explicitly. Every credential provider in this workspace keys on the reference
  alone, so different references mean different store slots.
- **Registry identity** — `zai` and `zai-coding` are separate provider rows
  with separate descriptors, so routing and setup cannot conflate them.

## Visible

`ZaiProfile::report` returns a `ZaiProfileReport`: plan, registry name, display
name, protocol, endpoint, the S04 `CredentialDescriptor` (reference, kind,
configured, source, provider, writable, validation) and any published usage
restriction. `summary()` renders one safe line, and `zai_plan_reports` returns
both plans side by side so the difference is legible rather than implied.

The report is built through `CredentialsService::describe`, never `resolve`
(GOTCHAS #33): a test asserts the credential provider's `inspect` ran and its
`resolve` did not. `CredentialDescriptor` has no value field, so no rendering
of a report can print a key; tests store real-looking secrets and assert they
appear in neither `Debug` nor `summary()`.

Coding Plan reports carry Z.ai's published restriction that the plan "may only
be used within officially supported tools and products" — heycode is not on that
list, and burying that in a design document would make a correct profile an
unsafe one.

## Sources

| Fact | Source |
|---|---|
| General base URL `https://api.z.ai/api/paas/v4/` and `Authorization: Bearer` | <https://docs.z.ai/guides/develop/http/introduction> |
| Coding Plan base URLs (Anthropic `/api/anthropic`, Chat `/api/coding/paas/v4`, Responses `/api/v1`) | <https://docs.z.ai/devpack/quick-start> |
| "The Team Plan Key is not interchangeable with other Z.AI's API Keys." | <https://docs.z.ai/devpack/quick-start> |
| "GLM Coding Plan may only be used within officially supported tools and products." | <https://docs.z.ai/devpack/usage-policy> |
| `export ZAI_API_KEY=your-api-key` | <https://docs.z.ai/guides/develop/python/introduction> |
| Model code `glm-5.3`; Coding Plan serves it | <https://docs.z.ai/guides/llm/glm-5.3>, <https://docs.z.ai/devpack/latest-model> |

`ZAI_CODING_API_KEY` is **not** a documented vendor name. Z.ai publishes no
environment variable for the Coding Plan key — its integration guides set the
host tool's variable instead (the Claude Code guide sets `ANTHROPIC_AUTH_TOKEN`,
which belongs to Claude Code). The reference is therefore heycode-owned and says
so in its doc comment, because inventing a plausible vendor name is exactly the
failure mode that looks correct until a user tries it.

## Shipping native route

CLI `provider = "zai"` now mounts `ZaiInference<General>` with an operation-time
credential binding and the exact general Chat endpoint. `ZAI_API_KEY` is the
default; custom references remain explicit. The Coding Plan reference cannot
back the general route. Native tools, background jobs, plan mode and portable
compaction use the same Agent as all other native providers.

`zai-coding` remains unavailable in CLI inference because the currently
published usage policy restricts it to officially supported products. Its typed
adapter and deterministic fixtures remain useful protocol substrate; they do
not assert heycode is an approved tool or relabel a general key as a subscription.

## PZA02 — maintained GLM catalog

Z.ai publishes **no model-list endpoint**. Its official OpenAPI document
(<https://docs.z.ai/openapi.json>) declares twelve paths and none of them lists
models. An unauthenticated probe cannot settle the question either: under
`/api/paas/v4/` *every* path returns the same `401 / code 1001`, including
paths that certainly do not exist, so a 401 on `/models` proves nothing. Under
`/api/v1/` a nonexistent path returns `{"code":500,"msg":"404 NOT_FOUND"}`
while `/api/v1/models` returns the auth error instead — so an **undocumented**
`models` route does exist behind auth on the Responses endpoint. Its response
shape has never been observed, so nothing here parses it.

The catalog is therefore *maintained*, and its evidence is that same OpenAPI
document — a machine-readable primary source rather than a prose table.

**Limits.** `max_tokens` declares `maximum: 131072`, which fixes `K` at 1024
for every per-family output cap the specification states, so each row's output
cap is a spec figure times 1024 rather than a rounded guess. The specification
publishes no context window at all, so a context window is recorded only where
Z.ai writes the exact digits in its own configuration guidance — `1000000` for
GLM-5.3, GLM-5.3-Flash and GLM-5.2 — and is `None` for every other row. A
rounded "200K" does not decide between 200,000 and 204,800, and guessing would
hand C11's context meter a number that looks authoritative.

**Capabilities.** `Supported` and `Unsupported` are used only where Z.ai states
the restriction: `response_format` is "Only text models support this field",
`thinking` is "Only supported by GLM-4.5 series and higher models", and vision
`tools` are "Only supported by GLM-5.3-Flash, the GLM-4.6V series, and
autoglm-phone-multilingual" — which is what makes GLM-4.5V's `tools`
*Unsupported* and GLM-4-32B-0414-128K's `reasoning` *Unsupported* rather than
merely unknown. AutoGLM-Phone-Multilingual sits outside the GLM-4.5-and-higher
numbering, so its reasoning is `Unknown` — neither stated nor excluded.
`document_input`, `native_web`, `native_compaction` and `prompt_cache` have no
per-model evidence and stay `Unknown`. The text tool union does admit a
provider-hosted `web_search` tool, but that is endpoint evidence rather than a
per-model claim, and PZA04 owns it.

**Retirement.** Unannounced is not absent. No Z.ai page publishes a shutdown
date, deprecation notice or replacement id for any model, so every row is
`ModelLifecycle::unknown()`. None claims `Stable`, and a test asserts that at a
far-future instant the effective status is still `Unknown` — absence of a
deadline never becomes a retirement, and it never becomes a promise either.

**Pricing** stays `unknown()`. Z.ai publishes prices on a documentation page,
not through any API endpoint, and a docs table is not API evidence — the same
line PAN01 and POA01 hold.

Both plans normalize the **same rows** under different provider identities.
Z.ai publishes one chat-completions model schema and no per-plan model list, so
a plan changes the endpoint and the entitlement, not the published model facts.
Omitting a model from one plan's generation would assert it is unavailable
there, and CAT03 treats absence from a complete catalog as a selection failure —
a claim Z.ai's documentation does not support.

## PZA03 — thinking/function-call continuation

GLM returns its reasoning as one `reasoning_content` **string** on the
assistant message, next to `tool_calls` — not a block type, not an opaque
signature, not a parallel array. So the replay unit is the whole assistant
message, which is exactly what C04's `ProviderStateKind::ChatAssistantMessage`
means, and that kind's validator constrains only `role`, leaving
`reasoning_content` to survive verbatim. **No `heycode-core` change was needed**;
the fit was checked against Z.ai's schema rather than assumed from the
Chat-Completions envelope.

Verbatim is the contract. Z.ai requires that all consecutive
`reasoning_content` blocks "exactly match the original sequence generated by
the model", and warns that reordering or editing them degrades performance and
cache hits. `ZaiInference<P>` therefore replays provider state untouched —
for **every** model — and the multi-step fixtures assert whole-message equality
against the value the response produced, so a replay that keeps the tool calls
and distils the thinking fails exactly like one that drops it.

### Which models are held to it

Enforcement is model-scoped, and that is a Z.ai fact rather than a hedge. The
`thinking.type` description says GLM-5.3 and GLM-5.3-Flash "can only be
enabled" and that "GLM-4.7 and GLM-4.5V will think compulsorily", but that
"GLM-5.2 GLM-5.1 GLM-5 GLM-4.6 GLM-4.5 and others will automatically determine
whether to think". A route that demanded `reasoning_content` back from GLM-4.6
would turn a legitimate thinking-free tool turn into a hard mid-session
failure.

`ZaiInference<P>` separates three routes by resolved model id: required
continuation with documented effort, optional continuation with documented
effort, and no effort field. For `ZAI_ALWAYS_THINKING_MODELS` it fails loud in
both directions: a tool-call turn replayed **without** its thinking is rejected
before transport, and a tool-call **response** that arrived without thinking
produces no provider state and no finish rather than state that is already
lossy. GLM-5.3/Flash use the shared continuation guard; older always-thinking
models use the same exact check locally because Z.ai does not document
`reasoning_effort` for them. For every other model that enforcement is off —
replay is still verbatim, but a thinking-free turn is accepted as the valid turn
Z.ai says it is. Only the ids Z.ai names by hand are on the strict list:
"GLM-4.7" may or may not mean its series, and a model wrongly listed loses
valid turns while one wrongly omitted only loses a safety net.

### Preserved thinking on the general endpoint

The shared Chat adapter now supports preservation independently of a reasoning
effort toggle. For maintained reasoning-capable models the route sends
`thinking: {"type":"enabled", "clear_thinking":false}`. It replays the full
ordered assistant message, including `reasoning_content`, unchanged. Models
whose reasoning is Unknown or Unsupported receive no thinking object. Effort
remains model-scoped: only GLM-5.2/5.3/5.3-Flash receive low/high/max; older
thinking models do not acquire a fictional effort setting.

`zai_thinking_reports` describes explicit preservation for both configured
adapters, while `ZaiPlanKind::PRESERVED_THINKING` continues to describe the
endpoint default. Temperature is rejected before transport unless finite and
in `[0,1]`. Credentials are acquired once per operation; the durable binding
names the actual configured reference, and rotation reaches the next request.

### Auth header: **Unknown** for the Coding Plan

Z.ai documents `Authorization: Bearer YOUR_API_KEY` for the general endpoint
and publishes **no** request header for the Coding Plan inference endpoints —
its integration guides only say to enter the API key in each tool. The shared
Chat adapter sends `Authorization: Bearer` either way, so the Coding Plan route
ships an unverified header. No live call was made to settle it (heycode holds no
Z.ai key, and Z.ai restricts Coding Plan keys to its supported tools), so
`ZaiAuthHeaderEvidence::Undocumented` travels with every Coding Plan thinking
report instead of the fact being silently upgraded.

### PZA03 sources

| Fact | Source |
|---|---|
| `thinking.clear_thinking` default `true`, "Controls whether to clear `reasoning_content` from previous conversation turns"; `reasoning_effort` supported by GLM-5.2 and above, enum/default/model mappings; assistant/response message schemas | <https://docs.z.ai/api-reference/llm/chat-completion> |
| Preserved Thinking needs `clear_thinking: false` plus complete unmodified ordered `reasoning_content`; enabled by default on the Coding Plan endpoint, disabled on the standard API endpoint; interleaved thinking returns thinking blocks with tool results | <https://docs.z.ai/guides/capabilities/thinking-mode> |
| GLM-5.3 `reasoning_effort` is `low`/`high`/`max` (default `max`); `thinking.type` supports enabled only | <https://docs.z.ai/guides/llm/glm-5.3> |
| `delta.tool_calls` carries `index`; `function.arguments` arrives as concatenated string fragments; `delta.reasoning_content` streams alongside | <https://docs.z.ai/guides/capabilities/stream-tool> |
| A finished turn is appended back as the assistant message plus `{"role":"tool","tool_call_id":…,"content":…}`; `function.arguments` is a JSON-format string | <https://docs.z.ai/guides/capabilities/function-calling> |
| `Authorization: Bearer YOUR_API_KEY` for the general endpoint | <https://docs.z.ai/guides/develop/http/introduction> |
| No request header published for the Coding Plan inference endpoints | <https://docs.z.ai/devpack/quick-start>, <https://docs.z.ai/devpack/tool/others> |

### Undocumented fields this route still sends

The shared Chat adapter always sends `stream_options: {"include_usage": true}`,
and `tool_choice`/`parallel_tool_calls` whenever tools are present. Z.ai's
request schema declares `tool_choice`, `temperature` and `max_tokens` but
declares **neither** `stream_options` nor `parallel_tool_calls`
(<https://docs.z.ai/api-reference/llm/chat-completion>). It also sets no
`additionalProperties: false`, so they are presumably ignored today — but they
are heycode sending fields the vendor does not document, and a stricter Z.ai would
reject every request. Not fixable from this crate: both are unconditional in
`heycode-llm`'s `chat_request_body`.

### Still **Unknown**

- Whether Z.ai's first streamed tool-call fragment carries `id` and `type`. The
  stream-tool page shows neither. The shared Chat parser requires `id` and
  `function.name` on the first fragment, so a stream without them fails loudly
  rather than yielding a call heycode cannot name — but the fixtures' frame shape
  on that one point is convention, not a quoted vendor fact.
- The request header for the Coding Plan inference endpoints (above).
- Z.ai types `function.arguments` as an *object* in its non-streaming response
  schema while its request schema and its own example code use a JSON-format
  *string*. heycode only streams and only ever emits the string form, so the
  inconsistency does not reach the wire — but it is unresolved.

## PZA04 — native web search

Z.AI publishes both a standalone `POST /api/paas/v4/web_search` API and a
Chat-level `web_search` result array. Both return the same seven result fields:
title, content summary, link, website name (`media`), icon URL, provider
reference and publication date.

`ZaiWebSearchClient` executes the bounded standalone API through the composed
HTTP service with an operation-time `RouteCredential`. `ZaiWebSearchRecord`
strictly validates the complete result generation, retains all seven fields,
round-trips them through its durable JSON format and rejects one unsafe row
atomically. `project()` emits one server-tool call, one result and one
unanchored citation per row. Each durable result source now carries optional
bounded web metadata, so site name, icon URL, provider reference and publication
date survive session JSONL. Debug and errors contain no query, URL, summary,
icon or provider body.

`zai_web_search_contribution()` owns the N01 identity
`provider:zai / web_search / zai:web_search` without importing the higher
`heycode-native-tools` crate. Non-default production factory `native-zai` maps that
validated route and priority into N01; a User profile proves exact selection,
inventory attribution and disposal.

The remaining product gap is inference activation, not durability. Z.AI puts
native Chat search metadata at the response top level and the shared Chat parser
still does not parse that route, but standalone native search now has a complete
durable/session path and a product-reachable N01 candidate. No live credentialed
search is claimed.

Sources: <https://docs.z.ai/api-reference/tools/web-search>,
<https://docs.z.ai/guides/tools/web-search>, and
<https://docs.z.ai/api-reference/llm/chat-completion>.

## PZA05 — Coding Plan MCP bundle

`ZaiCodingMcpBundle::official()` returns four secret-free server specs in stable
order:

| Server | Transport | Documented tools |
|---|---|---|
| `web-search-prime` | Streamable HTTP `https://api.z.ai/api/mcp/web_search_prime/mcp` | `webSearchPrime` |
| `web-reader` | Streamable HTTP `https://api.z.ai/api/mcp/web_reader/mcp` | `webReader` |
| `zai-vision` | local `npx -y @z_ai/mcp-server`; Node ≥22, package ≥0.1.2; `Z_AI_API_KEY` by credential reference and `Z_AI_MODE=ZAI` | eight documented image/video tools |
| `zread` | Streamable HTTP `https://api.z.ai/api/mcp/zread/mcp` | `search_doc`, `get_repo_structure`, `read_file` |

Remote servers bind `Authorization: Bearer` to the Coding Plan credential
reference and retain its exact `CredentialQuery` kind; no value exists in the
bundle or its snapshot. The local package is
recorded with a minimum version rather than silently executing unpinned
`@latest`. `mcp-zai-coding` dispatches all four through a host adapter inside one
core activation transaction, so a failed server removes the registered prefix
and stops the suffix.

Every server is explicitly optional, exact-tool-allowlisted, Prompt by default,
and exposes no resources, prompts or instructions. A root adapter therefore
does not invent policy defaults: remote references become Bearer bindings,
vision becomes one raw `Z_AI_API_KEY` launch binding plus the reviewed
`Z_AI_MODE=ZAI` literal, and all four enter `heycode_mcp::McpBoundServer` through
the ordinary policy-aware connection/generation/result owner.

`resolve_vision_launch` additionally requires a canonical executable/cwd,
Node.js major 22 or newer and an observed package version at least 0.1.2. The
accepted observed version is pinned into `@z_ai/mcp-server@<version>` argv;
non-canonical numeric spellings with leading zeroes are refused, and `@latest`
never enters the plan. The root adapter can map all three remote
Bearer definitions plus that raw vision binding into one four-server bound
plugin without values or inferred policy.

The bridge deliberately does not depend on `heycode-mcp`, as required by the
workspace dependency law. Product completion still requires root
factory/default-profile activation, mapping one policy-resolved local
executable, and real connection/tool discovery. No MCP server or `npx` package
was contacted here.

Sources: <https://docs.z.ai/devpack/mcp/search-mcp-server>,
<https://docs.z.ai/devpack/mcp/reader-mcp-server>,
<https://docs.z.ai/devpack/mcp/vision-mcp-server>, and
<https://docs.z.ai/devpack/mcp/zread-mcp-server>.
