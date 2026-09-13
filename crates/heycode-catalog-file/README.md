# heycode-catalog-file

Two file-backed halves of the model catalog, deliberately kept apart.

| Document | Written by | Read as |
|---|---|---|
| `$HEYCODE_HOME/cache/models.json` | heycode, after a successful provider refresh | provider evidence |
| override layers, e.g. `$HEYCODE_HOME/catalog-overrides.toml` | the user, by hand | user assertions |

The separation is the guarantee. Each reader can mint exactly one provenance,
neither document can spell its own, and heycode never writes an override document —
so a user assertion has no route back into the cache and can never be read on a
later start as something a vendor published.

## Catalog cache (schema v2; schema v1 readable)

Owner-only atomic whole-generation persistence behind `heycode_llm::CatalogPersistence`.
`0700` parent / `0600` file on Unix, 32 MiB cap, symlink refusal, explicit wire
mapping (never serde over the runtime structs), and a version probe before strict
decoding. It stores safe model metadata only: `[llm].provider` and `[llm].model`
stay in config as ids.

Schema v2 stores source and capture time inside each non-empty pricing or
performance object. Schema v1 never stored those fields; its rows remain
readable, but source-less advisory pricing/performance is restored as Unknown
rather than borrowing the enclosing generation timestamp or pretending the
cache knows the original source. A later live refresh can repopulate sourced
facts and the next save writes v2.

## Catalog overrides (schema v1)

```toml
schema_version = 1

# One entry per catalog row you are asserting something about.
[[model]]
provider = "openrouter"
model = "z-ai/glm-5.3-flash"     # canonical id; an alias does not match
context_window = 262144           # must be > 0
max_output_tokens = 65536

[model.capabilities]              # each value is exactly one of
tools = "supported"               #   supported | unsupported | unknown
prompt_cache = "unsupported"
```

Every table is `deny_unknown_fields`, and there is **no provenance key**: a
document says what you assert, never who asserted it. Writing `provenance = …`
or `revision = …` fails the load rather than being ignored.

Layers load in ascending precedence and resolve **per field**, so a project
layer that asserts `tools` does not erase a user layer's `prompt_cache`. Each
surviving field names the exact layer and path it came from, its zero-based
winning precedence, and the Unix-millisecond instant the immutable override
generation was captured.

Loading reads every layer once into an immutable generation. Later file edits
cannot change an existing `CatalogOverrides` or `AttributedCatalog`; an
explicit reload creates another generation with another capture instant. This
keeps provider refreshes and override reloads as independently captured
generations instead of silently re-reading a file while rendering a catalog
row.

An override attributes rows a provider published; it never adds one. An entry
naming a model the catalog does not list is reported through
`AttributedCatalog::unmatched()` as inert, not silently applied.

Anything malformed — an unknown capability word, a zero limit, a duplicate or
empty entry, a blank id, an unsafe path, an unsupported schema — fails the whole
load. A half-applied override set leaves a user believing something took effect
that did not.

## Reading an attributed catalog

`CatalogOverrides::attribute(&CatalogSnapshot) -> AttributedCatalog` borrows the
generation and never modifies it. Each row keeps the provider's descriptor
verbatim in `AttributedModel::evidence()`; assertions sit beside it.

There is no merged `ModelDescriptor`. `AttributedSupport` has two variants and
both name their origin, the payload types have private fields with crate-private
constructors, and nothing implements `Deserialize` or `Default` — so no code
outside this crate can build a capability value that does not say where it came
from.

**Display surfaces** must use `AttributedSupport::render()`, or match the enum
and style the two variants differently. `AttributedModel::assertions()` lists
every assertion on a row in stable field order — non-empty means the row is
overridden and must be marked as such. `AttributedCatalog::contradictions()`
lists assertions a live refresh has since disproved.

Machine enforcement is deliberately non-escalating. A user may narrow a
provider-evidenced capability or limit, but cannot turn Unknown/Unsupported
into Supported, invent an absent limit, or raise a published limit. The claim
remains visible—with its document plus the exact provider revision/capture it
stands against—while `CapabilityEnforcement` / `LimitEnforcement` retain the
provider fact. Redundant and narrowing policies retain the user source as an
explicit constraint.

`enforced_capability()` / `enforced()` return the resulting bare value with its
origin discarded. They exist only for request admission. Prefer the
origin-retaining `*_enforcement()` methods when building durable request/C05
facts, and **never render either machine accessor**: display surfaces must use
the attributed values so provider evidence and user policy cannot look alike.
