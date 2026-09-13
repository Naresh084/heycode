# heycode-runtime

`heycode-runtime` owns the provider-neutral `AgentRuntime` and `RuntimeSession`
contracts, the effect-owned runtime registry, opaque runtime ids, conservative
capability evidence and the strict raw-to-normalized event boundary.

`UnavailableAgentRuntime` is the shared optional-installation row used by
concrete runtime plugins. It preserves the implementation's immutable
descriptor, reports account state Unavailable, distinguishes Unsupported from
temporarily unavailable operations, and becomes Closed when its owning plugin
effect retires. It launches nothing and requires recompose after installation.

`RuntimeConfiguration` carries four exact, model-visible controls: system
prompt, ordered tool definitions, provider-native model id and provider-native
reasoning effort. Tool presence has a separate marker, so an explicitly empty
catalog disables inherited tools instead of collapsing to “unspecified”. Each
runtime advertises tri-state support for every field and rejects unsupported
fields by name before launch. A non-empty host tool catalog also requires a
`RuntimeToolExecutor`; the executor owns approval and execution and returns only
after both the call and its result are durable.

`RuntimeEventHub` is the shared sequenced fan-out every delegated adapter
publishes through — ACP here, plus the Claude and Codex runtimes. It validates
each emission with one session-lifetime `RuntimeEventNormalizer` before fan-out,
so an R02 violation is a `Protocol` error at the adapter's own call site instead
of a stream failure inside a distant consumer. It also bounds retention at
`RUNTIME_EVENT_HISTORY` events without ever failing an emission: sequence
numbers are per-subscription framing, so every subscriber is renumbered from
zero and eviction is free to drop history. Eviction only removes what nothing
retained still depends on — progress events, settled tool-call pairs,
interaction requests and then whole settled turns — so a trimmed window is
still a valid R02 stream beginning at session-ready. A session that streams far
more than the retention bound therefore neither dies mid-answer nor becomes
unsubscribable.

The generic ACP v1 boundary implements delegated sessions without importing a
process backend. `AcpProcessFactory` supplies one exact process connection;
the runtime owns that connection for initialization, session setup, prompt
turns, permission responses, cancellation and quiescent close. Raw stdout uses
strict bounded UTF-8 NDJSON framing with arbitrary-fragment support. Response
ids are exact, permission ids settle once, wrong or duplicate ids fail safely,
and provider-controlled error bodies do not enter runtime diagnostics.
The process factory receives separate connection-lifecycle and caller-operation
tokens: a completed start caller cannot later kill a published session, while
the owning plugin can synchronously cancel every connection generation before
its registry contribution is withdrawn. Session `close` remains the async
quiescent teardown boundary.

`opencode_acp_runtime` is the current OpenCode profile over that generic
boundary. It launches the documented `opencode acp` argv through the supplied
process Provider, reads session-scoped model values from ACP `configOptions`,
applies exact model and thought-level values through
`session/set_config_option`, and maps supported ACP updates into R02-normalized
events. Every selection requires the response's complete authoritative option
list and verifies its `currentValue`; a model change is applied before the
dependent effort control is re-resolved. Model discovery opens a
no-prompt ACP session and closes both session and process because ACP exposes
the catalog at session setup rather than through a standalone model method.

The OpenCode profile advertises only implemented evidence: models, resume and
permissions are Supported; fork, steer, follow-up, questions and compaction are
Unsupported. The production plugin adapts `heycode-exec` to `AcpProcessFactory`
and registers the runtime in the shared registry. Deterministic fixtures use
official `opencode-go/glm-5.3-flash` only to test exact selection; they make no
installed-OpenCode, credential or live-provider claim and never use the
retired anonymous Ox preview alias.

On 2026-08-31 an additional installed canary selected exact catalog row
`opencode/mimo-v2.5-free` and completed a credential-blind, no-tools,
content-withheld turn through this ACP boundary. It observed final settlement
and quiescent close without exposing text. This proves the live
session/event/model/provider bridge named by R10 on an official current
OpenCode route; it does not manufacture the absent GLM-5.3-Flash row or claim
anything about an authenticated account.

## Verification

```sh
cargo fmt -p heycode-runtime -- --check
cargo clippy -p heycode-runtime --all-targets -- -D warnings
cargo test -p heycode-runtime
```

## Connection setup update — 2026-09-05

Runtime discovery retains an effect-owned temporary workspace independently of actual session working directories. ACP accepts configuration-option catalogs and the legacy models/set-model contract, retaining which control produced the catalog.

Runtime descriptors may carry bounded provider-owned connection recovery instructions. The subscription wizard displays them after account or installation checks fail and keeps Enter available for retry; sign-in is performed through the official app.
