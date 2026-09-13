# heycode-cli

The default `provider-activation` plugin installs an inert factory for explicitly
selected fallback targets. It shares ordinary production provider constructors,
resolves only the target's own default credential reference, and admits the
model against that provider's catalog before registering it. It neither changes
the live route nor dispatches inference. Target providers use their official
product endpoints; local Ollama/LM Studio use their local defaults. Source
endpoint overrides and credentials never migrate to a different provider.

Azure, Bedrock, Vertex and custom endpoints need a separately configured and
registered connection. Google policies with hosted tools need primary
composition to own their native-tool registrations; late activation refuses
those settings instead of dropping them. A saved fallback authorization must be
prepared through the async activation API after each new composition, before
normal requests, rather than constructing a new route after an inference error.

`heycode-cli` is only the composition root. It resolves trust and configuration,
builds the factory table, composes plugins, selects TUI/headless/ACP/app-server/setup/doctor
mode and owns final Context shutdown.

The default Unix graph currently has 120 plugins. Root config schema v29 makes
`llm.protocol`, optional positive `llm.max_output_tokens` and selected
Settings-policy dependencies explicit;
DeepSeek Chat/Messages and Bedrock Mantle Responses/Messages never resolve by
an implicit nearby dialect.
The outer CLI exposes the same boundary as `--protocol <dialect>` and
`--max-output-tokens <positive>`, feeding the ordinary config patch validator
rather than maintaining a second parser/default.

`doctor --composition` has two explicit evidence phases. `inspect_world` keeps
the production graph strictly zero-apply. A healthy graph is followed by an
isolated production-loader activation in a disposable canonical workspace with
temporary product state, fake inference, watchers/resume disabled and configured
MCP transports suppressed. The report lists those substitutions, exposes only
plugin/scope/state/stage, and shuts down the activated Context before deleting
its root. It is not a credential, provider-network or MCP-health probe.

Credential, trust and named-profile changes return typed recomposition outcomes.
For `/profile`, the CLI replaces only the prior `--profile <name>` pair,
preserves every other original argument, shuts down the current Context, drops
its Tokio runtime and re-enters the authoritative startup load. The binary does
not carry a second profile resolver.

The named-profile production path also owns the K11 admission boundary.
`PluginFactories::build_profile` constructs descriptors, enforces the final
managed source/capability rules, and only then returns activatable scoped
plugins. A real-composition regression denies `external_process` and fails
before any Context exists. Legacy/profile-free startup has no managed policy to
apply and retains its existing path.

Session lifecycle recomposition replaces every existing `-c`, `--continue`,
`--resume <path>` or compact `-c<path>` selector with the exact committed
`<sessions-root>/<id>/session.jsonl`, preserves all other startup arguments,
then follows the same shutdown/runtime-drop/re-entry boundary.

Fresh composition mints its durable session id before plugin apply and passes
that one value to create-new session storage and every configured MCP router.
Resume binds the existing directory identity. `mcp_product_plugin` shares the
ordinary approval policy, form/URL TUI bridge and lifecycle-hook adapter;
`product-hook-attachments` runs after session/subagents/Agent and before TUI.
This is the production MCP11/MCP13 path, not a second UI-only broker.

## Headless output (`headless.rs`)

`heycode run` has one output contract in three formats. `text` (default) streams
the reply to stdout and puts tool activity (`⚙ bash ls -la`, `✓ bash (3
lines) …`), errors and a `session <id> · <reason> · tokens` trailer on stderr,
so `heycode run … | pbcopy` copies only the answer. `json` writes one envelope
`{session_id, reply, tool_calls, usage, reason, errors}` to stdout and nothing
to stderr. `stream-json` writes each durable session event — the JSONL v2
line, unchanged — to stdout as it commits. A non-terminal stdin is appended to
the prompt after a blank line (`echo SPEC | heycode run summarise`); a stdin that
is open but silent for 500 ms is ignored with a note rather than hanging the
run. Exit codes: 0 reply (including `max_tokens`), 1 error, 130 cancelled. The
printing subscriber is registered only for a headless run; the TUI never has
a stdout printer behind its screen.

Cross-crate product tests use `testing::RealCompositionHarness`, which retains
the isolated root until plugin effects have unwound and replaces only inference
with a sanctioned fake.

All built-in credential persistence uses `~/.heycode/credentials.toml`, or
`$HEYCODE_HOME/credentials.toml` when an absolute override is supplied. The
credential directory is `0700` and file replacements are `0600` on Unix.
Environment values are read-only overrides. There is no native keychain
provider, factory, dependency, initialization, or migration probe.

`check_new_key` covers every credential provider heycode can route to: DeepSeek
and OpenRouter through the shared bearer validator, OpenAI and Anthropic
through their own, and Gemini through `GoogleApiKeyValidator`'s
`x-goog-api-key` probe.

The main world, setup world and bootstrap helpers share environment/file
storage; the read-only doctor never opens a native secret store. Config schema
v29 replaces legacy complete-profile `credentials-keychain` rows with one
`credentials-file` row. Named/current profiles still selecting the retired
plugin fail with the exact replacement instruction. Existing OS entries are
never accessed or imported; keys held only there must be entered again through
setup or `/connect`. `heycode setup` runs
`check_new_key` against the provider (or the configured `llm.base_url`
gateway) before `write_credential_at`; a rejected key is retried, an
unreachable provider stores the key with a warning, and a cancelled or failed
wizard deletes everything it stored this run. Startup preflight reports a
rejected credential with `credential_source_label` and the next step, and
treats `Network` as a banner (`startup_warnings` on the TUI, stderr headless)
rather than a reason to force the wizard.

The restricted setup world remains metadata/catalog-only and now derives five
provider choices from real `ProviderProfile`s: Anthropic, DeepSeek, Google
Developer, OpenAI and OpenRouter. It constructs no inference client, session,
tool, watcher or MCP process. Catalog refresh remains user-triggered and missing
credentials produce the visible provider-default fallback rather than hiding a
provider or inventing a model table in the binary.

ACP permission forwarding is exercised through the real server loop and Agent
approval policy with a scripted tool-call provider. The duplex fixture proves
allow, deny and cancelled decisions, wrong/duplicate/late response-id safety,
exactly-once tool execution and a healthy later prompt. It performs no live
provider call and does not inspect credentials.

An ACP session subscribes to its runtime exactly once and every prompt pumps
that one stream. There is no cross-prompt sequence dedupe, because a runtime
subscription is contiguous from sequence zero for its whole life and a fresh
subscription is renumbered from zero once the hub has trimmed — a baseline
carried from an earlier subscription would sit above the whole replayed window
and silently drop every event. This matches the app-server side; see
`crates/heycode-app-server/README.md`.

The exact composition inventory includes both `settings` and the UI-neutral
`settings-ui` surface registry. Real-composition coverage follows TUI
`/settings` through the shared panel inbox into schema rows, so registering the
command without attaching the two services cannot be mistaken for a reachable
settings browser.

Default interactive composition mounts `plugin-lifecycle` beside
`mcp-management`, then mounts `panel-commands` after the TUI handle exists.
The owning plugins publish effect-owned MCP/plugin operation services over
Settings; standalone `heycode mcp`, standalone `heycode plugin` and the TUI consume
those same Context handles, and held handles become terminal after shutdown.
Plugin package facts come
from a fully verified cache snapshot and exact manifest readback; an absent
cache is empty, while unsafe/corrupt cache state fails rather than disappearing.

Default composition also mounts `compactions` before native Agent and subagent
Consumers. The binary only registers the factory/order; `heycode-agent` owns the
effect lifecycle and transaction. Schema v22 repairs historical exact profiles,
and real composition proves all three strategy descriptors disappear on Context
shutdown.

The production routing commands expose C14 recovery as
`/provider <id> portable|fork|cancel` and the same second argument on `/model`.
A direct picker/app-server switch across
opaque state refuses before Settings mutation. Portable resolution commits the
summary before route Settings/live publication; fork reports a durable child
and leaves the current world unchanged; cancel writes nothing.

Default `catalog-overrides` loads `$HEYCODE_HOME/catalog-overrides.toml` plus the
project layer only when the canonical Settings trust gate permits it. It
publishes assertions separately from `models`; TUI receives the optional handle
and labels them without rewriting provider evidence. Exact profiles may omit
the service. CAT07 still needs a durable attributed request/C05 enforcement
plane before an assertion changes execution.

The factory table may expose providers that are deliberately absent from the
default order. `telemetry-otlp-http` is one: a named profile must replace
`telemetry-local-off`, and real-composition coverage proves the opt-in service,
settings and inventory without making egress a default.

Standalone `heycode release apply|rollback` composes only an explicit off sandbox,
the ordinary local subprocess provider and `release-manager-gh`. Apply reads
four bounded absolute local bundle files, pins repository/workflow/OIDC/source
tag through GitHub's official verifier, snapshots enabled external-plugin API
ranges and then performs fresh/update policy plus atomic publication. Rollback
re-snapshots plugins, supplies current config-version evidence and re-verifies
every retained proof. No provider/session/TUI world or inherited verifier
environment is created.

`heycode release evidence <files...>` is the pure Q16 aggregation surface. It
accepts only bounded absolute non-symlink schema-v1 documents, reconstructs the
exact four checks for each hosted run and succeeds only when macOS, Linux and
Windows each carry one non-zero real-provider observation.

`heycode app-server --stdio-v1 --workspace <absolute-path> [--resume
<session-id>]` is the shipping X07 child-process host. The outer parser retains
global config/profile/provider/model/fake and explicit trust choices, then hands
the remaining closed arguments to a dedicated parser. Workspace must canonicalize
to an existing directory; resume accepts only canonical heycode UUID session ids
before joining the sessions root. With no explicit trust flag this noninteractive
surface defaults to restricted workspace authority; a reviewed editor setup may
place `--trust-workspace` before `app-server`.

The command composes the normal world, obtains the effect-owned AppServer and
gives stdin/stdout exclusively to bounded stdio-v1 frames. It installs no UI
stdout listener. EOF or Ctrl+C settles/cancels the transport, joins admitted
operations, then Context shutdown unwinds the ordinary server/runtime/Agent
effects. Raw frames and child stderr are never copied into product output.

The production OpenRouter factory always constructs the provider-owned
all-disabled transform policy. Routing and transforms are separate durable
options, independently verified from the session projection, and serialize to
top-level `provider` and `plugins`. No constructor used by the composition root
can omit transform intent and inherit gateway/account defaults.

Non-default factory `native-zai` maps the provider-owned PZA04 identity into
the shared native-tool registry. A User profile can opt it in; real composition
proves provider-only selection, exact inventory attribution and effect disposal
without claiming a Z.AI inference route or credentialed call.

Ollama uses a different conditional path. Only an explicit configured provider
and model registers `provider-ollama`; its four provider-owned services compose
after the model registry, and `llm` bridges the concrete no-credential Chat
provider into ordinary routing/pickers. Composition performs no request, daemon
start, pull or model selection. A configured credential reference fails before
lookup. The installed-model chat smoke remains PLM05 evidence, not a unit-test
claim.

Credential-backed production inference now includes OpenAI Responses and
Anthropic Messages alongside DeepSeek/OpenRouter. Both strict providers expose
the composed native-compaction operation; isolated real-composition tests use
unique owner-only test references and perform no network request. Portable
compaction stays the default. Provider-owned cache/context Settings default off,
resolve before registry publication, and detailed facts survive the neutral
durable/UI plane.

Google Developer API registers exact Search/code-execution N01 candidates for
maintained `gemini-3.7-flash`. Maintained Vertex Gemini/Claude catalogs perform
no network call and keep account access Unknown. Conditional lazy inference
plugins use `Provider::prepare_inference` to resolve the composed GCP profile
after model/N01 selection and before P10/C02/C05. AWS Converse/Mantle uses the
same joined phase to force private live evidence. No route blocks a nested
runtime or changes its endpoint inside streaming.

The production LLM factory retains only each configured credential reference.
Startup preflight may resolve once to diagnose/seed validation, then drops that
value; DeepSeek, OpenRouter, OpenAI, Anthropic and Google routes receive a cloned
registry-backed `RouteCredential`. Every request resolves the exact route once,
retries share only that operation's value and the next operation sees rotation.
Real composition pins strict authentication previews to the configured
custom handles, while the shared adapter matrix proves wire rotation and
foreign-route refusal before transport.

Every `llm` factory—fake, credential-backed and conditional Ollama—also owns
the exact `provider-interception` service plus request/response seam inventory.
Agent and subagent Consumers inject that service, so a strict route cannot gain
a composition-specific bypass while a compatibility route remains truthfully
outside the C02/C05 interception contract.

Optional default `provider-telemetry` contributes the body-free response
failure Consumer without making Agent depend on telemetry. Scoped factory
construction stably topologically orders concrete service providers before
their Consumers, so replacing local-off with an opt-in OTLP provider keeps the
same interception layer without a binary provider-id branch.

Optional defaults `request-transforms` and `request-transforms-openrouter`
publish N05's registry/P10 layer and the three provider-owned OpenRouter rows.
The existing production all-disabled option is therefore independently visible
and verified without moving transform ids, effects or prices into the binary.

On Unix, optional default `product-extensions` runs after the six destination
registries and subagent service. It lazily revalidates enabled cache objects and
activates declarative/MCP/code contributions transactionally. Root uses the
code-aware factory with an empty authority generation, so `[code]` packages
fail loud rather than downgrade. A profile requesting managed code authority
also fails during world resolution until a matching PL08 generation source is
composed; its fingerprint is never promoted into authority. A future trusted managed source must mint
entrypoint/runtime/session/grants/preopens/endpoints; none are inferred from a
manifest request. Non-Unix remains fail-closed pending owner security.

Default composition also registers `subagent-codex` and `subagent-claude`
after their delegated runtimes and the subagent registry, then attaches
`subagent-jobs` after Agent. The former are token-owned provider rows over the
generic R05/R08 adapter; the latter supplies O05's background host without
making the lower registry inject Agent. Config schema v25 repairs only exact
historical profiles that contain the corresponding Consumers/runtimes.

Six optional operational Consumers follow `agent` and `subagent-jobs` in the
default profile. `execution-jobs` binds the composed shell/terminal owners to
the existing JobRegistry and contributes `/tasks|ps|stop` plus background shell
and terminal tools. `goals`, `workflows` and `schedules` publish their own
effect-owned services and seven additional model tools over the same durable
session/Agent boundaries. The workflow Provider is the explicit
`SequentialWorkflowWorker`; the composition root does not implement workflow
semantics. `teams` adds an authority-scoped durable roster/task/mail service and
model tool. `reviewer` binds repository/worktree/session roots and contributes
the explicit selectable `/review-runtime`; it does not change the existing TUI
`/review` command's active-route meaning.

Those six rows did not require a config migration. No historical plugin injects
their services, so they are optional defaults rather than repaired dependencies;
adding them to an intentional exact profile would violate the profile's
selection authority. Profile-free startup derives the live default order and
therefore receives them immediately, while an exact profile may opt in by name.

O06 separately raises root schema to v26 because its optional
`subagent.worktree_base` is security-sensitive execution intent. When absent,
no worktree factory exists. When present, composition validates a full exact
Git commit before effects and conditionally registers three independent
providers—Codex, Claude and OpenCode—under owner state disjoint from the source
repository. DeepSeek Harness is excluded because its descriptor cannot prove
permission callbacks. The composition root never resolves branch names or
mutable HEAD as a fallback.

Default `telemetry-metrics` follows the selected telemetry service and the
session. It owns no metric interpretation in this binary: the Agent plugin
listens to the session's post-commit bus and contributes exact inventory rows.
The composition root only orders/registers that Consumer, so local-off and an
opt-in OTLP replacement see the same closed metric contract.

Interactive startup accepts `--screen-reader` and routes it to the TUI's
product-neutral flat mode. The flag is invalid for headless, setup, ACP,
doctor, MCP and plugin modes. Connection/profile/session/trust recomposition
retains it because those boundaries preserve every original argument except
the one selector they explicitly own.

QSEC02's cross-crate canary constructs the strict DeepSeek path over a
deterministic map-backed environment credential and mock transport. It proves
the value reaches the Authorization header, then checks the real request body,
projected prompt/auth snapshot, UI/debug stream, physical JSONL, process-spec
diagnostic and redacted support export. Each premise independently proves the
canary existed, preventing an unexercised boundary from passing by absence.

Default `status-context` follows `status` and contributes `/context` plus
`/usage` without adding a service key. Exact profiles may omit it. Production
inventory tests pin both command owners and the expanded `/compact`, `/provider`
and `/model` argument contracts.

Focused verification:

```sh
cargo clippy -p heycode-cli --all-targets -- -D warnings
cargo test -p heycode-cli
```

## Connection setup update — 2026-09-05

Connection changes restart with a fresh session and remove stale provider/model/protocol and resume flags, preserving config location and workspace policy. Credential repair preserves startup arguments. Pending API connections apply before provider composition. The invocation directory remains the actual session directory; pre-trust discovery uses a temporary directory.

The provider wizard preserves OpenRouter and includes Fireworks AI, Groq, Mistral AI, Together AI and xAI through provider-owned profiles, API-key flows, catalogs and strict Chat inference. `catalog-compatible` is part of the built-in profile.

Local connection startup restores the credential reference staged with the endpoint and model. Explicit CLI overrides still take precedence; choosing unauthenticated setup clears the unrelated prior reference.

Explicit Ollama credential references now construct the authenticated provider plugin instead of being rejected or ignored. Native catalog/inspector requests and Chat inference share the selected reference; the unauthenticated default remains explicit.

Startup retains routing coordinates from the same Settings read as provider/model selection. Saved AWS regions reach authorization, catalog admission, status and inference; the saved credential reference is shared by those consumers as well. Coordinates from a different explicitly selected provider are ignored, and unsupported coordinate names fail before activation.

Amazon Bedrock is reachable under the managed-cloud onboarding family. Its provider-owned region field drives isolated live discovery before any route change. Selecting a discovered model stages region/model/reference atomically and recomposes the production Converse route. Vertex and Azure remain absent from this picker until their separate connection phases complete.

Integration keeps exactly three welcome choices. Cloud and API profiles share Select a provider; cloud PTY checks reach their forms through provider search.


The endpoint integration regressions compose the real plugin world with Azure
and custom Chat providers over deterministic HTTP transports. They execute a
workspace `read`, replay its durable result, and verify that a rejecting
endpoint produces an error without a tool-free fallback. These are fixture
checks; live cloud accounts and custom servers are separate user checks.

The product is terminal-focused. AppServer supports the local `--stdio-v1`
transport; the HTTP listener, browser dashboard, remote host commands and
webhook endpoints were removed at the user's request on 2026-09-08. Native
terminal schedules, inbox delivery, workflows and Plan review remain available.
