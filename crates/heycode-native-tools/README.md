# heycode-native-tools

Effect-owned logical tool implementation registry and deterministic route
selection shared by provider-native, client-tool and MCP implementations.

N01 commits each selected logical→implementation route into the durable model
request header. N02 separately normalizes provider-executed call/result/citation
events while retaining exact provider blocks for replay. N04 adds user policy
modes beyond the initial native-when-matching, otherwise-client selection.

Registrations are Context effects and disappear at shutdown. Resolution groups
by logical id and chooses matching provider-native, then client, then MCP;
priority and implementation id make ties deterministic. Provider ownership and
all opaque ids are validated before publication. The built-in web Consumers
contribute `client:web_fetch` and `client:web_search` only when web is enabled.
`heycode-provider-openrouter` now contributes the first provider alternative,
`openrouter:web_search`; it wins only for the OpenRouter route and disappears
with its owning plugin effect.

N04 adds optional default plugin `native-tool-policy` and Settings namespace
`native-tools`. Its live default/per-logical values are `prefer-native`,
`prefer-local`, `native-only`, and `local-only`. Prefer modes fall back by
family; only modes fail before request commit when unavailable. Local ordering
is client then MCP. Invalid/unknown overrides and poisoned state fail loud, and
shutdown resets held registries to the original prefer-native behavior.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-native-tools --all-targets -- -D warnings
cargo test -p heycode-native-tools
```

Configured OpenAI and Anthropic hosted candidates now carry an exact model scope.
Agent request preparation and the final drift guard use `resolve_for_model`.
Changing to an unlisted model under prefer-native therefore selects the portable
client alternative; native-only remains an explicit failure. Unscoped `resolve`
remains available for provider inventory and backwards-compatible callers.
