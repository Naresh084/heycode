# heycode-core

`heycode-core` is the dependency-free internal microkernel: Context, Plugin,
effect lifecycle, events/waterfalls, opaque identifiers, exact contribution
inventory and provider-neutral shared vocabularies.

Exact inventory distinguishes `request_transform` implementations from tools,
providers and interception layers, so N05 provider rows can reuse logical names
without hiding ownership behind a generic plugin label.

`Waterfall::run_checked` preserves ordinary around-middleware ordering while
reporting whether the chain reached its terminal delegate. Policy seams use
that distinction to fail closed when a layer forgets `next`; an explicit typed
short-circuit remains separate from a middleware bug.

`PluginSource::as_str` and `PluginContributionKind::as_str` are the stable safe
policy identifiers used by K11. Source provenance and activation scope remain
independent: a built-in implementation enabled by a User profile is still a
built-in source, while its winning scope remains User.

K08 projects `ActivationReport` into a body-free diagnostic schema for the
composition doctor. It retains requested order, plugin id, scope,
`activated|failed|not_attempted` and failure stage, but structurally omits the
underlying error message. `CompositionDoctorReport` joins that activation phase
to the independently zero-apply graph report without conflating their claims.

MCP12 adds the durable rich tool-result vocabulary here because both session
storage and higher tool/provider Consumers need it without importing MCP.
`DurableToolResult` is schema-v1, closed and independently validated. It keeps
ordered blocks, attachment references, annotations/extensions,
presence-preserving structured JSON and honest schema-check evidence. Debug
reports shapes and sizes, never server text, URIs or media bodies.

PZA04 extends the existing durable server-tool source without a provider-specific
event kind. Optional `ServerToolWebMetadata` retains bounded site name, public
icon URL, provider reference and publication string. Legacy rows omit it;
Debug reveals only field presence.

N06 adds validated `ServerToolUsage`: positive provider aggregate request
counts with explicit evidence and Unknown or published non-zero pico-unit cost.
It is distinct from `ServerToolCall`; an aggregate can never manufacture call
ids, arguments or outcomes.

U16's detailed response vocabulary is provider-neutral and body-free.
`ProviderResponseMetadata` retains validated cache read/write/uncached/TTL/
reasoning counters plus ordered thinking/tool clearing facts and explicit cache-
prefix impact. Impossible partitions, duplicate edits and empty evidence fail;
Unknown is represented by absence rather than a zero.

Provider continuation state is a closed protocol/kind pair. Bedrock Converse
owns `BedrockConverseMessage`: one complete assistant role plus ordered
text/reasoning/tool-use union blocks. Opaque signatures and redacted reasoning
stay serialized for exact replay, while `Debug` for every `ProviderStateItem`
shows identity/schema only and always redacts data.

ATT04 extends the attachment vocabulary without adding a provider claim.
`AttachmentAudioMetadata` validates nonzero bounded duration, sample rate,
channel count and PCM depth; `AttachmentMetadata::new_audio` requires those
facts exactly for `audio/*`. Encoded samples remain structurally absent from
metadata, serde and Debug, while unknown wire members are rejected instead of
becoming a hidden byte side channel.

Focused verification:

```sh
cargo clippy -p heycode-core --all-targets -- -D warnings
cargo test -p heycode-core
```
