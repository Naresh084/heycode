# heycode-provider-minimax

MiniMax product/profile, catalog, and assistant-continuation boundaries.

PMM01 keeps pay-as-you-go and Token Plan credentials distinct at the type,
reference, kind, and registry-name layers. PMM02 discovers models through the
documented OpenAI-compatible and Anthropic-compatible list endpoints while
leaving capabilities unknown unless MiniMax publishes model-specific evidence.

PMM03 preserves complete assistant state for MiniMax interleaved-thinking tool
turns. `MiniMaxStateRoute` captures and replays schema-v1 `ProviderStateItem`
objects without rebuilding their JSON:

- native Chat responses retain the complete `<think>...</think>` content and
  function calls;
- `reasoning_split` Chat responses retain `reasoning_content`, every
  `reasoning_details` object, and function calls; and
- Messages responses retain the full ordered thinking/text/tool-use block list,
  including the opaque thinking signature required by the shared Anthropic
  adapter.

Capture and replay both reject dialect confusion, truncated reasoning,
duplicate tool ids, malformed tool state, the other MiniMax product, the wrong
model, or a provider-state protocol/kind mismatch. Unknown reasoning-detail or
Messages block types remain opaque and replay byte-semantically unchanged; they
are not promoted into a capability claim.

`MiniMaxInference<P>` now makes the Chat state boundary reachable from the
native Agent. CLI provider ids `minimax` and `minimax-token-plan` use the
international documented `/v1/chat/completions` endpoint and their separate
plan-owned credential references/kinds. Each operation re-resolves and admits
its key, including after rotation. A subscription key proves neither a seat nor
remaining Credits: account rejection is surfaced by the normal provider error
path, and no fallback spends a pay-as-you-go key.

The route explicitly sends `reasoning_split: false`, preserves complete native
`<think>` content in provider state, and separates thinking from visible text
only in streamed UI events. Truncated thinking and lossy neutral tool-call
history fail without a successful finish. Image detail uses MiniMax's documented
`default` spelling for the existing M3 image input capability. The adapter does
not invent reasoning effort values or change the catalog's Unknown tool evidence:
it explicitly attempts function-tool requests and still refuses an explicit
Unsupported model. Provider errors never retry with tools removed.

The international Chat path is the shipping direct route. Regional Messages,
split-reasoning streams and Token Plan MCP installation remain separate APIs;
no implicit protocol or regional switch occurs. Local `web_search`/`web_fetch`,
background tools, plan review and portable compaction come from the same native
harness as other providers, independent of any subscription runtime.

PMM04 provides a secret-free Token Plan MCP bundle description. It pins the
documented user-scoped `uvx minimax-coding-plan-mcp -y` launch, carries the
Token Plan credential as a reference/kind rather than a value, keeps
resources/prompts/instructions disabled, denies tools outside its exact
allowlist, and prompt-gates all external calls. The current Token Plan MCP guide
exposes both `web_search` and `understand_image`; `AllDocumented` installs both,
while `WebSearchOnly` is an explicit least-privilege restriction. Local resource
delivery also requires an explicit canonical directory.

The provider crate cannot depend upward on `heycode-mcp`, so
`minimax_token_plan_mcp_bundle_plugin` and `MiniMaxTokenPlanMcpHost` make this
an effect-owned provider plugin while leaving conversion with the composition
layer that can see both crates. That host must map the exact allowlist to
Prompt, exposure to none and the Token Plan query to a raw environment binding,
then use `heycode_mcp::McpBoundServer`/`mcp_bound_servers_plugin`. Values resolve
only when the child launches. CLI activation and real `uvx` connection plus
two-tool discovery remain root/live evidence gaps.

`MiniMaxTokenPlanMcpBundle::resolve_launch` now requires an already-resolved
executable regular file and canonical cwd, retains exact documented argv and
keeps `MINIMAX_API_KEY` as its original `CredentialQuery`. The cross-crate MCP
adapter can now map that launch into `McpBoundServer` with the exact allowlist,
Prompt policy, disabled non-tool exposure and raw operation-time binding; no
parallel connection or result owner is needed. Launch `Debug` reports only the
argument/environment counts; canonical paths, literal environment values and
credential references do not appear.

PMM05 adds an explicit coding-tool eligibility gate around the current Token
Plan profile. MiniMax documents that a Subscription Key may exist before the
user has any usable resources, so `MiniMaxCodingProfile::select` accepts only
an affirmatively assigned Token Plan seat or purchased Credits. It preserves
the existing Token Plan registry name, credential reference/kind, and only
documented regional protocol routes. Former Coding Plan documentation now
redirects to Token Plan and current guides use the standard `/v1` and
`/anthropic` endpoints; consequently this crate records dedicated-endpoint
evidence as `Unknown` and does not invent a third provider or URL. A concrete
account-entitlement inspector is still required before production selection.

Official contracts:

- <https://platform.minimax.io/docs/api-reference/text-openai-api>
- <https://platform.minimax.io/docs/api-reference/text-anthropic-api>

## Verification

```sh
cargo fmt -p heycode-provider-minimax -- --check
cargo clippy -p heycode-provider-minimax --all-targets -- -D warnings
cargo test -p heycode-provider-minimax
```
