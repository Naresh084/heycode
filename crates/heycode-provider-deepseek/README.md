# heycode-provider-deepseek

This crate owns DeepSeek-specific discovery and protocol facts. It contributes
the authenticated model catalog as plugin `catalog-deepseek` and exposes a
guarded Anthropic-format `Provider` over the shared `heycode-llm` Messages
adapter. Root composition does not select that provider yet.

## Protocol routes

DeepSeek publishes two independent inference dialects:

- The OpenAI-format origin is `https://api.deepseek.com`.
- The Anthropic-format SDK base is
  `https://api.deepseek.com/anthropic`. `DeepSeekAnthropicProfile` configures
  the shared Anthropic Messages adapter separately and pins DeepSeek's
  documented thinking/effort behavior. `DeepSeekAnthropicAdapter` is the
  provider-owned dispatch guard: it refuses names the gateway would silently
  replace with `deepseek-v4-flash`, and requires documented Claude aliases to
  be canonicalized before exact dispatch.

The compatibility table explicitly supports `x-api-key`. DeepSeek's Claude
Code recipe instead sets `ANTHROPIC_AUTH_TOKEN`; that implies the current
Claude Code bearer projection but does not document a DeepSeek bearer-header
wire guarantee. The profile therefore exposes that integration shape while
keeping `AuthorizationBearerHeader` at `IntegrationInferred` / `Unknown`.
Production consumers can supply a `RouteCredential` so the key is acquired
once per operation and rotation reaches the next request; fixed keys exist for
embedding and deterministic tests.

The operation-time claim is now exercised through the complete guarded route,
not only by inspecting its `ResolveSpec`: one adapter dispatches two Messages
operations through a rotating resolver, resolves exactly once per operation,
sends the first and second `x-api-key` values on their corresponding requests,
and retains only `AuthenticationBinding::Credential(reference)` in the resolved
contract. No secret appears in provider Debug or failure output. Root can use
the same path with `RouteCredential::registry(credentials, query)`; no new
DeepSeek-specific credential abstraction is required.

Provider/profile/catalog identity remains multi-dialect, reflecting DeepSeek's
published Chat and Messages surfaces. The selected inference adapter narrows
that identity to exactly `AnthropicMessages`, matching its resolve spec and
preventing strict dispatch from treating a broad provider descriptor as a
route.

The Anthropic profile deliberately does not promote undocumented response
facts. DeepSeek does not publish the Messages path below its SDK base, the
`thinking.signature` behavior, streaming event names, stop/usage response and
cache-counter fields, or its error envelope. The deterministic parity fixtures
therefore use constructed Anthropic streams; they prove that heycode preserves
signed thinking, tool calls, and the next tool-result request, but they are not
a DeepSeek wire capture. CAT08 metadata makes that distinction structural: the
fragmentation fixture is labelled `Synthetic`, names the official compatibility
guide and its review version, and carries a non-zero reconciliation instant
through every generated split case.

The live parity test runs only when `HEYCODE_E2E=1` and a non-blank
`DEEPSEEK_API_KEY` are both present. Its first request must finish with one
client tool call plus nonempty signed thinking; the exact parser-published
state and matching tool result are then replayed into a second request, which
must finish with `Stop`, nonempty text, and the exact requested
`deepseek-v4-pro` identity (so a silent Flash fallback cannot pass).
Failure output is a closed enum with no credential, response body, normalized
text, tool arguments, or call ids. Without that two-leg observation, PDS04's
live-smoke acceptance clause remains open even when every deterministic package
test passes.

This crate does not register the Anthropic route into the product world. Root
composition still needs an explicit DeepSeek dialect selection that constructs
`DeepSeekAnthropicAdapter` with the composed `HttpService` and a registry-backed
`RouteCredential`, registers it as the selected `Provider`, and preserves the
strict request/header/provider-state path. The adapter advertises its exact
inference boundary and refuses legacy Chat fallback; it still cannot select
itself in root composition. Until that owner lands, the lower profile is tested
but not product-reachable.

## Optional endpoint capabilities

PDS05 models four surfaces independently; none is inferred from ordinary Chat
support:

- Strict tools use `https://api.deepseek.com/beta/chat/completions`. The
  provider option records explicit activation and the provider projection adds
  `strict: true` to every ordinary logged `ToolSpec`. DeepSeek remains the
  authority that validates its evolving supported JSON Schema subset.
- JSON Output uses `https://api.deepseek.com/chat/completions` with
  `response_format: {"type":"json_object"}`. Admission checks the
  documented, case-insensitive `json` prompt word. Whether prose contains a
  useful desired-format example remains a product-facing requirement, not a
  guessed parser rule.
- FIM uses `https://api.deepseek.com/beta/completions`, is fixed to the model
  admitted by the current endpoint schema (`deepseek-v4-pro`), rejects output
  limits above 4K, and exposes no Chat or thinking fields. The pricing table
  marks V4 Flash FIM-capable while the endpoint schema lists only V4 Pro, so
  Flash remains `Unknown`; the disagreement is not resolved optimistically.
- Chat prefix uses `https://api.deepseek.com/beta/chat/completions`. Prefix
  content remains an ordinary logged assistant message; a separate durable
  marker requires the eventual adapter projection to place `prefix: true` on
  that final message.

These are provider-owned lower contracts, not yet composed product features.
The shared Chat adapter still needs explicit strict-tool and final-prefix
projections plus distinct standard/beta route instances, and FIM needs a
Completions consumer. Until those patches and root registrations land, the
surfaces are not product-reachable even though their endpoint/request contracts
are tested here.

Official evidence reviewed 2026-08-31:

- <https://api-docs.deepseek.com/guides/anthropic_api/>
- <https://api-docs.deepseek.com/quick_start/agent_integrations/claude_code/>
- <https://api-docs.deepseek.com/guides/thinking_mode/>
- <https://api-docs.deepseek.com/guides/tool_calls/>
- <https://api-docs.deepseek.com/guides/json_mode/>
- <https://api-docs.deepseek.com/guides/chat_prefix_completion/>
- <https://api-docs.deepseek.com/api/create-completion/>
- <https://api-docs.deepseek.com/guides/fim_completion/>
- <https://api-docs.deepseek.com/quick_start/pricing/>

## Verification

From the workspace root:

```text
cargo fmt --check -p heycode-provider-deepseek
cargo clippy -p heycode-provider-deepseek --all-targets -- -D warnings
cargo test -p heycode-provider-deepseek --no-fail-fast
```

Current deterministic inventory: **80 tests** (3 unit + 77 integration). The
credential-gated two-leg smoke remains externally unobserved when
`HEYCODE_E2E=1` and a trustworthy `DEEPSEEK_API_KEY` are unavailable.
