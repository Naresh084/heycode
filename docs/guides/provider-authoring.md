# Authoring a heycode inference provider

This guide is the current production path for a provider whose model loop is
owned by heycode. Subscription coding agents such as Codex or Claude Code implement
`AgentRuntime` instead; do not make them inference adapters and do not reuse
their private login tokens as API keys.

The acceptance unit is a vertical route, not a crate. A provider is not shipped
until its auth, catalog evidence, exact inference/replay, optional native
features, composition and safe verification all join through `Context`.

## 1. Choose ownership and crate boundaries

Create `crates/heycode-provider-<id>` only when current code needs the provider.
Add it to `[workspace.dependencies]`, inherit workspace lints and document its
row in `AGENTS.md` before code lands.

Provider crates may depend only on lower contracts they consume—normally
`heycode-core`, `heycode-http`, `heycode-llm`, `heycode-credentials`, `heycode-authorization`
and `heycode-authorization-api-key`. They do not import `heycode-agent`, `heycode-tui`,
`heycode-session` or the CLI. If a provider needs a new durable fact, add a neutral
lower vocabulary first and let Agent/session own the commit.

Use stable ids consistently:

- provider registry/catalog id: lowercase kebab-case;
- credential reference: provider-owned, non-secret metadata;
- authorization flow: `<provider>-api-key` or an exact non-key method;
- protocol: one represented `ProviderProtocol`, never `Unknown` on strict I/O;
- model id: provider spelling preserved; aliases are explicit catalog data.

## 2. Profile and authorization

Implement a `ProviderProfile` whose registry name, `ProviderDescriptor.id`,
default model and optional credential reference agree exactly. Setup and pickers
read this metadata; they never contain a provider table.

An API-key provider contributes an authorization flow through the shared
registry. The flow owns:

1. masked input through `secret-prompt`;
2. a provider-specific validation request with a closed failure taxonomy;
3. registry-owned credential commit and authoritative safe readback;
4. cancellation before and after validation/commit;
5. an effect disposer for its registration.

Never print or return key values, authorization headers, response bodies or raw
transport errors. Resolve credential references per operation when rotation is
expected. A provider object may use `RouteCredential` so Debug remains redacted
and compaction/normal inference share the same acquisition contract.

Cloud ambient chains and subscription auth need their own authorization/runtime
owner. Do not force them into the API-key flow.

## 3. Catalog and evidence

Implement `ModelCatalog` and register it with `CatalogRegistry::register` from a
provider-owned plugin. `fetch` receives one cancellation token and returns a
whole candidate generation. Validate every row/cursor/page before publication;
one malformed row rejects the generation and never replaces last-good.

Normalize only evidence the source actually publishes:

- `Supported`, `Unsupported` and `Unknown` are independent facts;
- gateway-provided features may be evidenced by the gateway; model-intrinsic
  features require model evidence;
- lifecycle, retirement, aliases, limits and replacements are explicit;
- pricing/performance are advisory and any non-empty value carries
  `ModelMetadataProvenance { source, captured_at_ms }`;
- missing price, limit or capability never becomes zero/false;
- a protocol supporting a field does not prove a composed model supports it.

Catalog refresh uses the shared bounded HTTP service and operation-time
credential reference. Classify status/transport/shape without returning the
body. Provider registration performs no catalog request.

Add the source plugin to the CLI factory table and default/named profile only
when its dependencies are explicit. Historical exact profiles gain it through a
semantic schema migration only when an existing Consumer requires it.

## 4. Strict inference and request resolution

Implement `Provider` for discovery/metadata and expose `InferenceAdapter` for
production dispatch. Once an adapter is advertised, `Provider::stream` must fail
loud; falling back to a legacy route would bypass durable verification.

`InferenceAdapter::resolve` is the provider's pre-network admission point:

1. validate provider/model/protocol identity and catalog lifecycle;
2. accept only represented tools/media/reasoning/native features/options;
3. materialize provider-owned defaults explicitly;
4. validate same-route `ProviderStateItem` continuation requirements;
5. call `resolve_request` with one exact `ResolveSpec`;
6. return one ownership-consumed `ResolvedCall`.

Do not hide defaults in serialization. Agent converts the resolved call into
`request/header` and `request/context`, appends them, independently re-projects
the log and compares every field before `stream_cancellable` can run.

The adapter stream emits normalized `InferenceEvent`s and obeys exact terminal
ordering. Provider state/server events are buffered until successful Finish;
failed/cancelled output cannot influence a later request. One caller token owns
transport attempts and retry waits. Stateful/native calls disable replay unless
the provider proves replay safety before any normalized output.

## 5. Lossless continuation and protocol fixtures

Anything required on a later call becomes `ProviderStateItem` with exact
provider, canonical model, protocol, kind and schema. Keep opaque signatures,
encrypted reasoning, phases, thought signatures, compaction blocks and unknown
complete extensions byte-semantically unchanged. Another provider never parses
or receives them.

Session projection includes same-route state and retains a neutral assistant
fallback when incompatible/partial state cannot replace it. A complete state
item may suppress the duplicate neutral assistant representation only through
the shared replacement rules.

Streaming protocol tests use
`heycode_llm::testing::{SseConformanceFixture,SseFixtureCase,run_sse_conformance}`:

- retain raw network chunks;
- exercise whole, bytewise and every-boundary fragmentation;
- pass through the production `SseDecoder`;
- inject terminal transport failure without fake EOF;
- assert transport calls and normalized terminal/state output;
- include multi-step tool/reasoning continuation, malformed state and
  cancellation.

## 6. Provider request options and native features

Provider-owned policy that reaches the wire becomes a bounded
`ProviderRequestOption`. Configure an exact adapter wire dialect; unknown kinds,
duplicate fields, reserved collisions and unprojected siblings fail before
transport. A provider-local JSON builder alone is not product activation.

Native tools register candidates through `NativeToolRegistry` with logical id,
implementation id, provider owner and evidence. Agent resolves one route,
removes the corresponding client schema only when a provider route wins and
commits the choice in `request/header`. Normalize provider calls/results/
citations through the shared server-tool events while retaining exact replay
state separately.

Native compaction implements optional `NativeCompactionAdapter` below Agent.
The resolved call has `CallPurpose::Compaction` and exactly
`NativeFeature::Compaction`; the operation returns a bounded same-route
`NativeCompactionCheckpoint`. Agent's `compactions` registry alone appends
`compaction/native`. Portable remains the default unless a human/policy selects
the native row. A provider switch across opaque state uses the C14
portable/fork/cancel policy.

Prompt-cache/context-edit controls also need durable settings and detailed
neutral usage/metadata events before their rows are complete. An in-memory
provider ledger or UI-only badge is not restart evidence.

## 7. Token and cost evidence

Token counters register through `TokenCounterRegistry` and describe scope plus
evidence. Use:

- `Exact` only when the provider documents an exact count;
- `Estimated(ProviderTokenizer)` when a provider tokenizer endpoint documents
  an estimate;
- `Estimated(Utf8ByteRatio)` only for the local fallback.

Represent every supported structured input (tools/results/media) or refuse it;
never flatten/drop content to return a smaller number. Cache read/write,
reasoning and original/effective context components remain distinct neutral
facts. Missing detailed usage or price is Unknown, not zero/free.

## 8. Composition and reachability

Register provider-owned auth/catalog/native-tool plugins in the factory table.
For an inference route, add the id to the exact production inference set,
remove it from configured-only, map its default credential reference/name and
construct its strict provider inside the credential-backed `llm` plugin over
the composed HTTP service.

Composition must perform no provider request. Prove reachability with
`heycode_cli::testing::RealCompositionHarness`:

1. disable the sanctioned fake explicitly;
2. select the provider/model in isolated Config;
3. seed a unique owner-only test credential reference (never environment/global
   keychain state);
4. compose through the production factory/loader;
5. assert provider/profile/catalog descriptor agreement, strict adapter/native
   operation presence and inventory ownership;
6. shut down Context before deleting the temporary root.

Protocol tests plus a composition test are both required: the first proves
behavior, the second proves a real world can reach it.

## 9. Verification and acceptance

Before submitting a provider change:

- write red protocol/admission/replay/cancellation/body-canary tests first;
- run crate formatting and warnings-denied all-target clippy;
- run provider package tests once, then only diagnosed exact failures;
- run affected shared adapter/Agent/session/composition tests;
- regenerate `docs/reference/capabilities.md` if route classes changed;
- update the provider README and relevant public guides;
- record what was not observed (live credential, platform, account feature).

Live evidence uses an isolated route and never prints content/credentials. A
configured invalid credential is a failure, not a skip. `Supported` product
tier additionally needs real auth/catalog/text/tool/error/compaction evidence
and current platform gates; deterministic fixtures alone are Preview evidence.

## Final checklist

- [ ] Profile/descriptor/default/reference agree.
- [ ] Authorization commit/readback is masked, cancellable and effect-owned.
- [ ] Catalog generation is all-or-nothing and Unknown-safe.
- [ ] Strict resolution rejects unsupported/unproven choices before I/O.
- [ ] Exact provider state replays across at least three turns/tool steps.
- [ ] Stream/error/cancellation settlement is closed and body-free.
- [ ] Native tools/options/compaction cross shared durable boundaries.
- [ ] Token/cache/cost evidence is typed honestly and visible after restart.
- [ ] Production composition reaches the provider without network I/O.
- [ ] Focused gates and safe live evidence satisfy the row's exact acceptance.
