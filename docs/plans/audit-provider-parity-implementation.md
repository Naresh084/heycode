# Provider parity implementation

Workstream branch: `codex/provider-parity-20260908`, based on the clean shared
snapshot `174740a54b664ae2df2150ea2373f0718ea4201b`. This is implementation for
A35 and the cross-provider native delivery of A01–A37; it does not close the
other workstreams' lifecycle or UI findings by proving schema transport alone.

## Implemented paths

- CLI mounts MiniMax PAYG and Token Plan Chat inference and catalogs, plus Z.ai
  general Chat inference. Credential types/references stay distinct. MiniMax
  rechecks key admission per operation; Z.ai supports rotating exact references.
- MiniMax uses explicit native think-tag output, preserves full assistant state,
  and separates only the UI text/reasoning events. Truncated/lossy continuation,
  wrong-product keys, invalid sampling and explicit tool incapability fail before
  successful settlement. M3 uses documented image detail `default`; M2 vision
  remains unproven.
- GLM reasoning-capable models receive preserved-thinking control independent of
  effort vocabulary. General API reasoning is no longer cleared by endpoint
  default. Unsupported/unknown reasoning models get no fictional toggle.
- Gemini transports native JSON Schema via `parametersJsonSchema`, preserving
  unions/object constraints instead of assigning them to Google's narrower
  Schema message.
- Compatible gateways attempt Unknown function tools while keeping catalog
  evidence unchanged and refusing explicit Unsupported. Local LM Studio no-auth
  mode emits no dummy bearer credential and retains loaded-model preparation.
- Configured OpenAI/Anthropic hosted tool candidates have exact model scope.
  Model-aware preparation/drift checking selects portable fallback after a model
  change under prefer policies; native-only remains fail-closed.
- Anthropic ordinary requests default to at most 8192 output tokens (clamped to
  model metadata). Portable compaction is distinguished from native compaction
  by NativeFeature::Compaction, not request purpose alone.
- Portable summary collection accepts real adapters' plain-text/reasoning replay
  envelopes without persisting them. Tool/server/non-text state remains rejected.

## Public APIs and neighboring integration

- `MiniMaxInference<P>::new(HttpService, CredentialsService, MiniMaxProfile<P>, String)`.
- `ZaiInference<P>::with_credential`; `ZaiPreservedThinking::ExplicitPreservation`.
- `OpenAiChatCompletionsConfig::{with_preserved_thinking, with_reasoning_split,
  with_image_detail}`; existing defaults unchanged.
- `NativeToolImplementation::with_models`, `NativeToolRegistry::resolve_for_model`.
- Agent `agent.rs` and `provider_consumers.rs`: model-aware N01 preparation and
  final admission. `compaction_registry.rs`: text-state summary admission. These
  are narrow neighboring changes required to make provider delivery work.
- Added default catalog plugin rows require integration's generated inventory
  and exact default composition expectations to include both MiniMax catalogs.

## Deterministic evidence and failures found

`provider_protocol_matrix` in heycode-cli uses actual production adapters, the real
Agent and shipping tool registry. Each route executes a file read, compares every
wire tool schema with the durable request's schema, checks tool-result/state
replay, then sends a second human turn and requests portable-summary compaction.
Routes: OpenAI Responses, Azure Responses, Anthropic, Google, DeepSeek, GLM,
MiniMax PAYG/Token Plan, Ollama, LM Studio, OpenRouter, custom Chat endpoint,
Fireworks, Groq, Mistral, Together and xAI. Fixture capability declarations are
explicitly synthetic; no live account or model-quality claim follows.

The matrix found real defects missed by schema-only tests: MiniMax's adapter
advertised both catalog protocols instead of exact Chat; Anthropic had no
ordinary-turn output default and treated every summary as native compaction;
portable summary collection rejected all real text responses' ProviderState.
All 19 final tests pass, including all 17 complete tool-turn/summary routes and
two CLI route/policy composition checks. Initial fixture-only failures (missing coordinates,
wrong session service wrapper, absent hosted policies/opaque reasoning, one-turn
fold with no eligible prefix) were corrected without weakening production gates.

Final deterministic validation:

- MiniMax: 134 tests; Z.ai: 71; Anthropic: 91; native-tool registry: 8.
- Shared LLM: 442 tests, including Chat/Gemini/retry protocol regression.
- CLI composition filter: 65 tests, including production retry composition.
- CLI library: 28 tests; generated capability reference updated and checked.
- Agent compaction registry: 4 tests, including hidden tool-state rejection.
- Strict Clippy passes for all targets of CLI, Agent, LLM, native-tools and the
  six changed provider crates (MiniMax, Z.ai, Anthropic, OpenAI, compatible,
  LM Studio). No warnings.
- Compatible/LM Studio existing suites passed before final focused additions;
  their final production wrappers also pass the real Agent matrix.
- MiniMax lifecycle fixtures cover exact state replay, rotating credentials,
  truncated thinking, wrong-product keys, cancellation, explicit tool
  incapability/lossy history, M3 image detail and every UTF-8 split-marker boundary.

## Inherent limits and root follow-through

- Z.ai Coding Plan stays restricted: the current official policy permits only
  supported tools and heycode is absent. CLI gives an actionable restriction and
  never spends a different entitlement. Its adapter substrate is not production
  authorization. MiniMax subscription resources are account-authoritative; a key
  does not guarantee an active seat or remaining Credits.
- Direct MiniMax shipping route is international Chat; regional Messages/split
  reasoning and MCP installation remain separate, unactivated integrations.
- Tools, background execution and plan permissions are harness features for
  tool-capable models. Explicitly tool-incapable models cannot be made to call
  tools faithfully by inventing intent; no vision/reasoning support is fabricated.
- Root owns final integrated new-tool lifecycle tests and full UI/PTY checks.
  The matrix compares all advertised schemas dynamically and will cover schema
  additions after integration; it does not simulate background jobs by name.
- L17 remains root-owned beyond same-route retry. Verified RetrySpec bounds
  attempts and stops after ANY normalized output (including ResponseStarted),
  reuses one exact route/body/operation credential, respects provider veto and
  cancellation. Cross-route fallback needs explicit configured target(s), durable
  failed-request settlement, no partial output/tool dispatch/opaque state, exact
  safe definitive failure classes, a finite attempt budget and visible routing.
  It must not be implemented by changing transport retry's credential or endpoint.

## Primary contracts reviewed 2026-09-08

- https://platform.minimax.io/docs/api-reference/text-openai-api
- https://platform.minimax.io/docs/token-plan/other-tools
- https://docs.z.ai/guides/capabilities/thinking-mode
- https://docs.z.ai/devpack/usage-policy
- https://ai.google.dev/api/generate-content#FunctionDeclaration

No paid credentials or live inference used. The workstream is ready for root
integration; L17 and the integrated
new-tool/UI lifecycle remain explicitly outside this provider delta.

## L17 follow-up: explicit provider activation prerequisite

The read-only review of root's first fallback implementation found that the
production registry contained only the selected inference provider. Catalog
and connection-profile presence could not make a second provider dispatchable.
The follow-up adds `ProviderActivator`, `ProviderRegistry::install_activator`
and async `ProviderRegistry::activate(provider, model, cancellation)`. The
default CLI `provider-activation` plugin installs an inert, effect-owned factory;
only an explicit target choice or previously saved authorization may invoke it.

Ordinary composition and late activation share one credential-provider builder.
Targets use their own credential references, product endpoints and Settings
namespaces. The factory proves model selection through the target catalog before
publication. Existing registered providers are never replaced. Cancellation,
owner disposal, foreign provider identity and competing registration cannot
publish or remove another owner's route. Registry activation changes neither
the selected Agent route nor routing settings and sends no inference request.

Supported fresh targets are OpenAI, Anthropic, DeepSeek, OpenRouter, MiniMax PAYG
and Token Plan, Z.ai general, Gemini Developer, five compatible gateways, and
local Ollama/LM Studio. Cloud deployment products and custom endpoints require
an independently configured connection already registered in the world.
Google hosted-tool policies require primary composition to own their native
registry contributions; late activation refuses them instead of ignoring them.

New evidence uses production CLI factories with an isolated HTTP substitution:
DeepSeek retains a source-only key and custom endpoint while activated Anthropic
uses its own API key/header/official endpoint and options. Both execute Agent
read/tool replay. Negative coverage proves missing target key, unevidenced
model, incomplete deployment and restricted subscription refusal. An activated
Anthropic hosted-tool request retains `RetrySafety::Never` and makes exactly one
HTTP attempt on a definitive 503. That response is correctly normalized as
Overloaded; the initial fixture expected Server and was corrected. The full
matrix had 21 passes plus that fixture assertion, and its corrected focused test
passes, covering all 22 final cases. Four activation ownership/cancellation/race
tests and all 65 CLI composition tests pass. Strict all-target Clippy for CLI,
LLM and Google passes, with a final LLM check after the last ownership test.

Root owns the combined fallback gate, settings snapshot CAS and async saved
authorization preparation. Recommended preparation uses an effect-owned
pre-step layer before the first normal request, so both headless and interactive
entrypoints can await activation; it must not construct a route from the failure
handler. Saved target failure should make fallback unavailable while preserving
the usable primary route. No root worktree files were edited in this follow-up.


## Cloud wrapper Agent matrix follow-up

Five additive journeys now use actual production wrappers: Bedrock
ConverseStream, Mantle Responses, Mantle Messages, Vertex Gemini, and Claude on
Vertex. They share the direct-provider matrix's real composition/Agent path:
advertise all model-facing tools, execute `read` against an isolated workspace,
replay its result, answer another human turn, then compact with portable summary.
Mandatory built-in tool presence and every advertised schema are checked against
the durable request header.

Each HTTP operation verifies the exact endpoint and product auth headers.
Converse uses checksummed binary AWS event-stream frames; the other four use
scripted SSE. The first tool response includes provider-specific opaque state:
Converse/Claude thinking signatures, Responses encrypted reasoning, or Gemini
thought signatures. Persisted state protocol and complete state JSON must match
an exact subtree of the second request. Claude Vertex also asserts the body
version and absence of a body model identifier.

This journey exposed a production Converse defect: validation rejected the
Agent's client/MCP native-tool selections before dispatch. Those selections now
remain in the resolved call for local execution; provider-hosted selections
continue to fail because no hosted dialect is configured. A focused resolver
test verifies both local implementation kinds and the hosted refusal remains.

Catalog evidence is synthetic. Mantle's catalog source is explicitly registered
by the test because ordinary production composition installs it only with AWS
coordinates; no credentials or cloud coordinates are inferred from the host.
This matrix exercises inference wrappers, not live authentication or full cloud
onboarding. There is no shipping InvokeModel/InvokeModelWithResponseStream
adapter in the current source, so no InvokeModel coverage is claimed. No paid
requests or subscription-route changes were made.

Validation completed:

- `cargo test -p heycode-cli --test main cloud_provider_matrix`: 5 passed.
- `cargo test -p heycode-cli --test main provider_protocol_matrix`: 24 passed.
- `cargo test -p heycode-llm --test main bedrock_protocol`: 61 passed.
- `cargo clippy -p heycode-cli -p heycode-llm --all-targets -- -D warnings`: passed.
- Changed Rust files pass `rustfmt --check`; `git diff --check` is clean.

This additive delta is based on root integration snapshot `92ffd2a`.
