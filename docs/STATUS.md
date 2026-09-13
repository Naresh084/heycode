# STATUS — current stage & how to continue

The 2026-09-06 terminal/runtime integration now includes the visually reviewed
conversation redesign: an animated orange cat, active backend/model/effort
header, shaded user turns, assistant bullets, an inline multiline composer and
two compact footer rows. The branch/PR is green without a redundant label, the
concrete model is purple, measured input/output counts are cyan, and the context
meter stays separate from billing usage. Approval mode and shortcuts occupy the
second row; effort sits above the composer on the right. `/settings` persists pet visibility, header
density and footer choices. Effort choices form a vertical dropdown backed by
the active runtime's advertised values. Runtime-link chatter stays out of the
conversation; diagnostic and accessible projections retain its information.
Activity sits immediately above the composer with working, reasoning, responding,
tool and user-wait phases plus elapsed time. Concrete reported model IDs take
precedence over catalog aliases, survive sparse effort refreshes and clear on
a different selector; a real Claude startup shows `claude-opus-5[1m]`.

The shared `ask_user_question` host tool supports native tool-capable models and
delegated adapters that expose configured host tools. It waits for a selected answer, custom text or explicit
cancellation, with one owning surface and serialized concurrent questions.
Question prompts remain interactive in automatic approval mode. The TUI shows
labelled choices and descriptions, an Other entry and a waiting-for-answer phase.

Claude host tools now preserve the utility PATH under the guarded execution
policy. ToolSearch selects heycode host tools, approvals round-trip through the
app-server, and todo updates reach the session task summary. `/permissions full_access` allows actions
without prompting; `/permissions default` asks each time.
Shift+Tab cycles Full access, Accepted edits and Default through that same policy owner, preserves
the draft and shows the committed mode without adding transcript chatter.
Typing `/exit` or the unknown `/exist` cannot execute before Enter. Delegated
startup identifies Claude instead of the dormant OpenRouter provider, and the
accepted bare `opus` selector inherits metadata only from a unique advertised
qualified Opus model.

`/logout` now disconnects the active backend and returns immediately to the
full Welcome screen. The saved connection is cleared and a durable setup
requirement prevents automatic reconnection on restart, including environment,
project and vendor-login fallbacks. Native logout removes the exact saved
writable credential, including optional local-server keys. Cleanup failures
remain visible in Welcome while inference stays disconnected. Explicit setup
starts a fresh conversation. Real Claude PTY checks cover logout, restart and
fresh reconnection; an authenticated LM Studio-shaped HTTP fixture covers
masked key setup, deletion and restart. Vendor CLI login stores remain intact.

`/model` and `/effort` discover and configure the active backend. Delegated
`runtime_model` and `runtime_effort` persist separately from the dormant native
provider/model/effort tuple and reach startup/resume. Supported runtime adapters
receive the composed system prompt and host tools; guarded host tool execution
commits calls/results and rejects duplicate active or settled correlations.
Native controls validate the exact adapter effort vocabulary before publishing.
Omitting a delegated model never selects the native fallback implicitly.

The new v2 `runtime/configured` event distinguishes attempted, committed and
failed configuration; legacy rows default to committed. Requests are recorded
before child dispatch, only committed updates enter UI replay, and failed first
launches remain retryable. Configure validates an already-open session identity
before mutation and closes children whose successful configuration cannot be
committed to the log. A post-wire Settings CAS/storage failure retires the exact
backend generation and requires reopening from the durable tuple; delayed
cleanup cannot close a newer replacement. See [the routing contract](plans/model-effort-routing-contract.md)
and [runtime contract](plans/subscription-runtime-controls-contract.md).

Final verification on 2026-09-06: `cargo fmt --all --check`, workspace all-target
warnings-denied Clippy, generated-reference/documentation verification and
`cargo test --workspace --no-fail-fast` pass: 4,170 tests, zero failed or ignored, followed by passing focused permission/layout regressions.
The TypeScript SDK passes all eight tests; the Python suite passes all nine,
including eight real-binary full-screen PTY scenarios. The production binary
was rebuilt. A Codex cancellation-test timing oracle was corrected to release
the descendant only after cancellation returns; its full 32-test suite and
concurrent probes pass, with no production cleanup change. A separate Codex
request race now preserves an already delivered correlated response if a later
protocol frame closes the connection; later failures remain observable through
events and subsequent requests. Its regression passed 48 concurrent runs.

Live Claude verification now includes guarded host tools and real terminal
interaction: one read approval, Full access shell execution with exit code zero,
task-list progress, advertised effort selection and retained Claude identity.
The shared question tool was exercised under Full access: a selected choice, typed
custom answer and explicit cancellation each produced the expected durable tool
result and resumed Claude. Card and custom-input captures were visually checked.
Raw SDK and app-server observations establish an authoritative 1,000,000-token
capacity and latest-request input/cache counts. The context meter uses this
measurement separately from aggregate billing usage; older results without
capacity continue normally with unknown context. A real chat capture shows
`5.7k / 1M` in the footer. The current production renderer was visually inspected
against the supplied references; see [design QA](../design-qa.md).

Custom tool transport on other subscriptions remains fixture-tested rather than
claimed live everywhere. ACP custom system/tool controls and DeepSeek Harness
model discovery remain explicitly unsupported where the upstream boundary
cannot provide them.

Connection onboarding expansion is implemented at the requested three-entry boundary: subscription, local model and provider (including cloud), with bounded provider/model search. Saved routing plus missing/rejected credentials enters targeted Reconnect instead of the first-run Welcome; transient network failure continues unverified. LM Studio/Ollama have a server URL field and isolated draft discovery. Amazon Bedrock owns a required region form, Google Vertex AI owns project/location plus external ADC/OAuth readiness, and Microsoft Azure OpenAI owns resource/deployment plus exact v1 API-key readiness and Responses inference. The local hierarchy also exposes one custom OpenAI-compatible Chat server with an explicit version-root URL, optional bearer reference and discovered-or-explicit model. Production restart aligns coordinates, references, catalogs and inference without changing process environment. All five isolated implementation phases are integrated with the primary-checkout review corrections; broader onboarding work remains tracked in [implementation tasks](plans/connection-onboarding.md). See [the completed phased plan](plans/deferred-cloud-local-connections.md).

OpenRouter is preserved. Fireworks/Groq/Mistral/Together/xAI now have provider-owned strict Chat routes, masked flows and `catalog-compatible`; metadata/catalog/production-adapter checks and raw SSE fragmentation conformance pass. New `ConnectionProfile` keeps family, optional model fallback, credential reference, selection policy and local help separate from an active inference provider. Local entries do not invent defaults. Fireworks discovery retains serverless shutdown dates instead of treating READY as ongoing availability. LM Studio uses exact loaded-instance ids and rechecks loaded/tool/Chat readiness before inference; Ollama's catalog is reachable before selection and distinguishes running/downloaded models. The custom server uses only canonical `/models` and `/chat/completions`; listings prove identity but leave all capabilities Unknown. Azure/custom routes explicitly attempt ordinary function tools when evidence is Unknown, preserve the logged schemas and results, and surface endpoint refusal without a tool-free fallback; known Unsupported and other unknown capabilities remain rejected (GOTCHAS #352). Local endpoints were not reached during this revision, and live Fireworks/Groq turns are unobserved.

The preceding cloud/local baseline contained 62 crates and 120 default Unix plugins, including setup-safe `catalog-azure-openai` and `catalog-custom-openai`; the exact production inventory test passes. No new service or session kind. Final integration passes `cargo fmt --all --check`, workspace all-target warnings-denied Clippy, generated-reference/documentation verification and `cargo test --workspace --no-fail-fast`: 4,055 unit/integration tests plus 7 doctests, 4,062 total, zero failed or ignored. The primary-review correction adds four real-composition Azure/custom Agent read-tool and rejection tests plus two provider admission guards. All pass; both provider guards kill a compiling Unsupported-admission mutation, followed by exact restoration and the final green workspace run. The isolated restricted-workspace `--fake` smoke settles with the expected content-free stop, and the four focused production-binary PTY journeys for Bedrock, Vertex, Azure and custom Chat setup pass together. The reachable connection-copy audit removed one Azure future-auth teaser and found no mock/placeholder provider rows or runtime-management claims. Phase 4 separately passed all 12 provider-openai-compatible tests, 235 TUI tests and 207 CLI tests; its compiling tool-capability mutation was killed and restored. Tool-free live subscription turns for Codex, Claude Code and Grok remain observed from the preceding revision. Cloud/custom behavior here is fixture-backed and production-reachable, but no live cloud account, custom endpoint or credential was used.

2026-09-05 credential storage change: native keychain crate/dependency/factory removed. All built-in persistence uses `~/.heycode/credentials.toml` (or the absolute `HEYCODE_HOME` override); schema v29 migrates old complete-profile selections. At that storage milestone the workspace had 58 crates and 114 default Unix plugins. No new service or session kind. See GOTCHAS #326.

Verified on this macOS host: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --no-fail-fast` pass (3,931 unit/integration tests + 7 doctests, 3,938 total, zero failed/ignored). Focused CLI/config suites pass 263 tests; `scripts/tui_blackbox.py` passes 5/5 full-screen scenarios. A compiling mutation retaining the retired plugin was killed by the migration regression, followed by byte-exact source restoration and a passing rerun. Generated references and documentation checks pass.

An authorized real-home PTY launch reached **Welcome to heycode** immediately after backing up and upgrading `config.toml` from v28 to v29. The stored OpenRouter **file** credential was classified unauthorized and the UI offered Connect. This is live startup evidence, not successful model inference; no OS store was read, written, imported or cleaned up.

Historical tracker milestone: **2026-09-05, home-file credential storage.** The exact tracker stands at **254 complete · 31 active · 7 not started · 0 blocked** across a **58-crate** workspace and **114 default plugins on Unix**. PAWS05, MCP11 and MCP13 are newly accepted. Schema 28 repairs exact-profile dependencies for Settings-backed OpenAI/Anthropic/AWS/Google policy owners while profile v3 retains managed code authority without fabricating its PL08 generation. Root now shares one durable session id and approval policy across session/MCP/TUI/hook adapters; `hook/contribution` is v2-only and durable-before-model. O09, PMM04/PZA05, PL09/PL10, provider/live/platform/attestation rows and QSEC01 retain their observed authority/evidence gaps. The full shipping graph now has a measured compact build without turning local size evidence into hosted release evidence. No active row is promoted merely because its lower implementation is green.

The previous schema-v28 integration milestone passed 3,924 unit/integration tests plus 7 doctests. The current schema-v29 gate is recorded above. The provider/platform/attestation evidence gaps below remain independent of this local storage fix.

The isolated shipping binary also passes: doctor is healthy with four checks,
the fake headless turn settles `[done:stop]`, and raw stdio app-server v1
completes initialize/open/turn-stop/close. Documentation reports four generated
references fresh, 55 links, 18 shell examples, one deterministic example and
three accessible diagrams green.

The documentation lane and the two Node distributables now have CI definitions:
`.github/workflows/docs-lane.yml` (DOC04/DOC05, runs `scripts/tests` plus
`verify_docs.py` with no `--write`) and `.github/workflows/node-packages.yml`
(X06, path-filtered on `sdks/**` and `editors/**`, Node 24, `npm ci`/`npm test`/
VSIX package). Both headers state plainly that they have never executed on
GitHub Actions, so like `cross-platform-gates.yml` they are definitions and not
evidence (principle #14, GOTCHAS #182). X07's installed Extension Development
Host journey stays a human-run acceptance gate; nothing automated runs
`test:installed-host` and `node-packages.yml` deliberately does not claim it.
`python3 scripts/verify_docs.py` is green on this tree: generation is now a
separate `--write` step, and the gate itself never writes the bytes it compares.

The shipping build now uses a separate compact `dist` profile and records the
exact staged byte count before attesting those same bytes. On the current
macOS-aarch64 host, the unchanged full-capability graph measures 63,961,264
bytes under ordinary release and 18,609,680 bytes under `dist` (70.9% smaller);
gzip-9 is 10,529,053 bytes and zstd-19 is 9,096,990 bytes. The isolated 1.4-GiB
distribution cache is disposable and the resulting binary passes the fake
headless turn. The workflow enforces a 20-MiB ceiling only for this observed
platform and emits unbaselined size documents elsewhere. This is local artifact
evidence, not a hosted Q14/Q15 transaction or Q16 platform pass.

The side-effect-free setup world now derives Anthropic, DeepSeek, Google
Developer, OpenAI and OpenRouter choices from provider-owned profiles and real
catalog contributions. It still creates no session/Agent/tool/TUI/MCP process;
missing credentials produce the visible provider-default fallback. AWS/Vertex
remain explicit cloud-profile routes rather than receiving an invented setup
default.

The 2026-08-31 release-manager, delegated-runtime and operational-domain work
is newer than that historical release batch. Current evidence is heycode-install **67** tests
plus all-target warnings-denied clippy; heycode-cli **6** release-surface tests
plus the outer parser, exact service/plugin/tool/command composition checks and
no-deps warnings-denied clippy; heycode-runtime **26**,
heycode-runtime-opencode **9**, heycode-runtime-deepseek-harness **9**,
heycode-agent **232**, heycode-session **190** and heycode-exec **89** tests; Unix
script syntax, one native deterministic release smoke and YAML parsing.
All parallel tasks have returned; the post-integration milestone gate above is
the current accepted workspace evidence.

**Delegated runtimes now share one sequenced fan-out.**
`heycode_runtime::RuntimeEventHub` and `heycode_runtime::RUNTIME_EVENT_HISTORY` are the
single bounded fan-out for every delegated adapter (ACP, Claude, Codex); three
private per-adapter `EventHub` implementations were deleted. Retention is
structure-aware rather than a blind `pop_front`, and every subscription is
renumbered from zero — per-subscription framing, not session identity
(GOTCHAS #299/#300). `heycode-app-server` and `heycode-cli`'s ACP session each hold one
subscription for the session's life and do no cross-turn sequence dedupe.
`crates/heycode-agent/src/native_runtime.rs` holds the same shared hub
(`events: Arc<heycode_runtime::RuntimeEventHub>`); no hand-rolled hub remains, so
there is no outlier left to unify.

**The session store no longer fails whole-store listing on one unprojectable
session.** Store integrity (symlinked/non-regular entry, corrupt line,
unterminated tail, unreadable directory) is still fail-loud for the whole scan;
a well-formed log this build cannot project is one unreadable row.
`SessionSummary::is_readable()` marks it, `--continue` and the picker keep
working, and resume/fork/rename/archive/export still refuse loudly for that
target. The split is stated verbatim in `crates/heycode-session/README.md`
("Query and forks") and in GOTCHAS #308. TUI surfacing is now complete: the
session browser draws `unreadable` in both full (`render::draw_session_browser`,
error-colored marker plus `unreadable` status instead of the misleading `empty`)
and flat (`accessibility::render_sessions` appends `— unreadable`) modes, pinned
by `unreadable_row_is_visibly_damaged_in_full_and_flat_modes`; no
session-summary rendering was found in `heycode-app-server`, so no second surface
remains.

**Integration-test module registration was swept after the lane recovery.**
`crates/heycode-session/tests/it/mod.rs` was missing 12 of its 24 modules — the
files existed on disk and never compiled, so their tests were silently absent
from every run. They are wired in now, and a sweep of every
`crates/*/tests/it/mod.rs` against the files beside it finds no remaining gap.
A `.rs` file in `tests/it/` that no `mod` declares is invisible, not failing;
re-run that comparison after any lane recovery.

**The TUI ask plane holds concurrent approval dialogs** (queue plus on-card
count) instead of a single slot, and quit bounds its wait on the turn task
(5 s settle, then abort, then detach). Both were carried as deferred agent-lane
findings and are now closed in heycode-tui.

**Provider composition coverage has a known blind spot.** Every openai/anthropic
real-composition test in `crates/heycode-cli/tests/it/composition.rs` pins the one
model constant the capability table admits (`:247` `OPENAI_GPT_5_6_SOL`, `:507`
`ANTHROPIC_CLAUDE_OPUS_5`), which is why the suite stayed green while any other
model id aborted composition outright (GOTCHAS #298). A composition test with a
non-flagship model id is the right guard and belongs in the heycode-cli lane.
Residual, not fixed here: `heycode doctor` reports `healthy` and
`[ready] native-openai@0.1.0` without ever running plugin `apply()`, so it
cannot detect an activation-stage failure of this class — it reported healthy
for the configuration that could not boot.

**Configured MCP servers and added MCP servers are one set.** The product world
adopts `[mcp.servers]` rows into `McpManagement` and merges
`McpManagement::connectable()` into the connection set, so `heycode mcp add` takes
effect in the next session and configuration wins a name collision. The
standalone `heycode mcp list/remove/enable` world still composes only Settings plus
its own plugin and therefore sees added servers only; the in-session `/mcp`
panel sees both.

**Shipped Anthropic and OpenAI routes derive replay safety from the narrower of
neutral resolution and protocol-specific adapter evidence**, not from resolution
alone. Any future provider route composing through `resolve_request`
out-of-crate must do the same or it re-opens that defect class (GOTCHAS #307).

Two retry-policy follow-ups stay open and each needs **two crates in one lane**,
which is why neither landed here. (1) heycode-agent + heycode-llm: put
`retry_spec: RetrySpec` on `ResolveSpec` and thread each adapter's configured
policy through `config.resolve_spec()`, so an out-of-crate adapter with a
non-standard policy inherits it instead of re-applying `with_retry_spec`; the
struct literals in `crates/heycode-agent/tests/it/{request_invariant,adapter_activation}.rs`
break on any added field. (2) heycode-agent + heycode-session: the `retry` row on
`RequestOptionsSnapshot` was already filled from `call.retry_spec()` in
`snapshots_from_resolved_call` but nothing compared it. `compare_header` now
pins `retry_max_attempts` exactly whenever the durable row recorded it (legacy
headers without the row skip the check instead of reading absence as a policy,
pinned by `legacy_header_without_a_retry_row_still_verifies` and
`retry_attempt_budget_drift_fails_before_transport`). Safety itself stays
rank-bound by design — an adapter narrowing from protocol evidence the header
does not carry still verifies — so C05 now bounds replay safety AND pins the
attempt budget, while a safety difference with no footprint in
`native_features`/`native_tool_routes` beyond narrowing stays refused by the
rank check. The clean home for the narrowing rule itself is
`ResolvedCall::narrow_retry_spec(RetrySpec)` taking the min of the two safeties;
adding it should delete `heycode-provider-anthropic`'s local
`narrower_safety`/`replay_rank` helpers in the same change. Full exact-policy
equality (which would also pin safety, not just the budget) still waits on (1).

**Q10/Q11 are accepted at their literal harness boundaries.** Five repeated
debug-product samples record startup p95 35.4 ms, local TTFT 526.4 ms, 1K replay
568.8 ms, flat render 562.4 ms and 1K-tool registry composition 559.7 ms under
enforced CI regression ceilings. These are not release targets. Matched fixture
agents each pass 9/9 deterministic tasks; Wilson and paired-bootstrap reports
correctly return `insufficient_evidence` below the 30-pair decision floor. No
live superiority or release-performance claim is made.

**QSEC05 now passes the real black-box product gate.** A red Rust regression
proved the strict Tool input copied raw durable content after computing its
source wrapper. The shared sink now consumes the derived content; Web, MCP and
LSP regressions pass while an unlabelled tool result remains byte-exact. The
rebuilt binary receives each hostile source inside its exact warning, issues a
real write request, durably denies it, leaves no marker and cleans up; the
separate auto-approval control creates its marker. The content-free artifact
validator passes.

**Q13's deterministic chaos framework is complete; Q12 is not.** Five closed
settlement scenarios pass with byte-identical fixed-seed reports. The complete
local fuzz smoke passes session/config/provider/MCP 128-run cells and the
8-run render cell after its ANSI/OSC corpus seed forced a production Markdown
sanitizer. Isolated fuzz/chaos clippy and formatting are green. The scheduled
hosted continuous matrix has never run, so Q12 remains active.

**X07 is an installed shipping-binary proof, not an activation mock.** The CLI
owns a closed `app-server --stdio-v1` mode with canonical workspace, UUID-only
resume, restricted-default trust, protocol-exclusive stdout and joined Context
shutdown. An isolated installed VSIX opens/sends/overlaps cancel/closes, resumes
the exact session in a fresh process, completes a healthy turn and disconnects.
Extension tests pass 7/7 including permission correlation; Rust app-server 17,
SDK 6 and the CLI binary smoke are green.

**Cloud request/state composition is product-reachable without synchronous
discovery shortcuts.** Agent materializes catalog/model/N01 facts, then runs one
caller-cancelled `Provider::prepare_inference` future before adapter/options/P10
and C02/C05. AWS lazy providers force private live evidence and independently
resolve catalog/inference credentials. Vertex lazy providers resolve the
composed GCP profile and return an exact endpoint-bound operation provider;
maintained Gemini/Claude catalogs keep account access Unknown. Google Developer
registers exact Search and code-execution candidates; implicit cache activation
was deliberately withheld because its live catalog evidence is Unknown.
DeepSeek's Chat/Anthropic Messages dialect is schema-28 explicit. Hosted/live
and provider-specific policy rows remain active where their acceptance says so.

**MCP11/MCP13 are product-complete; provider bundles and concrete O09 handlers
remain open.** HTTP and MCP tests prove response head precedes a
pull-owned bounded body, open SSE can await a concurrently owned elicitation
reply POST, progress/logging continue, and cancellation sends no late reply.
Root shares one durable session route and ordinary Agent approval policy with
the TUI/lifecycle adapters. MiniMax/Z.AI add canonical pinned launch admission,
but bound provider factories/live generations keep PMM04/PZA05 active; O09
still needs concrete structured handler providers.

**PLM05/R10/R12 have stronger live gates but no honest model turn.** LM Studio/
Ollama **111**, runtime **26**, OpenCode **9** and Harness **9** tests pass with
warnings denied. No Ollama executable/daemon exists. OpenCode 1.18.21 is
hash-bound and its credential-blind catalog handshake passes, but the catalog
does not expose official `opencode-go/glm-5.3-flash`, so the no-tools/content-
withheld turn is not sent. The pinned Harness checkout has no built server/tsx,
needs pnpm 11.7.0 while 9.15.0 is installed, and has a modified lockfile; its
launcher/artifact hashes are stricter, but no unsafe build is attempted.

**PL09/PL10 now have an installed product path but remain active for managed
authority configuration.** Exec **89**, extensions **140** and extension-host
**24** tests pass with warnings denied. Manifest `[code]` metadata, exact
provenance/session/grants/resources, six real registry adapters, native process
retirement and import-exact Wasmtime/WASIp3 checks are concrete. Root uses the
code-aware installed factory with an empty authority generation: code packages
fail loud rather than downgrade to declarative activation. A trusted managed
grant/preopen/endpoint source remains open; PL11 remains QSEC01-gated.

**O06/O07/O14 are product-reachable.** Agent **232** and
Session **190** tests plus exact root composition/inventory gates pass. Default
services `teams` and `reviews` expose the `team` tool and selectable
`/review-runtime`: team CAS/DAG/mailbox state commits before job/A03 notices;
reviews commit exact tracked base/patch/instructions, run DenyAll in an isolated
checkout, reject mutation and publish only structured findings. Optional schema
26 `subagent.worktree_base` validates a full commit before effects and
conditionally composes Codex/Claude/OpenCode worktree Providers; absent config
composes none and never resolves mutable HEAD.

**C12 is product-reachable and complete.** Default effect plugin/service
`compactions` contributes native, portable and prune rows. Strategies prepare
read-only plans; the registry proves no strategy/concurrent log mutation and
alone commits one settlement. `/compact`, automatic pressure and native runtime
all use `portable-summary`. Optional strict provider transport returns an exact
route checkpoint that commits as v2-only `compaction/native`; same-route replay
uses it, while neutral/incompatible routes retain original history. Schema v22
inserts the service before historical Agent/subagent Consumers. Provider-specific
OpenAI/Anthropic native compaction and provider policy activation are complete
through POA04/05 and PAN04/05/06.

**C15 durable stress is complete.** One real 1,000-turn/9,011-event session
commits complete request/provider correlations and ten alternating portable/
native checkpoints, then reopens. Sequence remains contiguous,
`project_requests` reconstructs all 1,000 calls, exact OpenAI input retains the
final checkpoint plus ten recent provider messages, and Anthropic input contains
neutral history with no OpenAI opaque state. The 0.66-second local run is not a
performance claim.

**C14 provider-switch policy is product-reachable.** Session projection exposes
the winning opaque barrier and exact pre-checkpoint fork count. Direct routing
and model changes refuse before Settings mutation. Human command
`/provider <id> portable|fork|cancel` either appends a current-provider portable
summary then commits the route, creates a resumable child while leaving the
current world unchanged, or writes nothing. Equal boundaries are later-wins,
and turn completion is correlated by id rather than guessed from the last event.
App-server v1 maps a choice-requiring request to Conflict.

**K11 managed admission is complete.** Standalone profile schema v2 remains
backward-readable for v1 plugin rows and adds a constraints section that only a
Managed source may bind. It can allow exact implementation sources and deny
broad descriptor capability families. The production factory constructs the
descriptors, enforces the final policy and returns activatable plugins only
after every row passes, so a forbidden source/process/tool cannot publish even
one effect. PL08 is now dependency-ready.

**U15/CMD06 session lifecycle is product-reachable.** Default TUI contributes
the bounded sessions panel and queued `/new|resume|fork|rename|archive|delete|
export` commands over the effect-owned JSONL query service. Filters and rows
retain storage/lineage/source/runtime/cwd/status evidence; corrupt entries fail
the whole page visibly. Archive keeps fork paths stable, delete defaults to
cancel and moves only a closed leaf to owner-controlled trash, and lossless
export validates each physical suffix before bundling its ancestors. One Unix
lineage lock serializes fork/delete/restore/export across processes. CLI
recomposition preserves all original arguments except the exact resume target;
schema v23 repairs historical exact TUI profiles. C10 still owns redacted
support bundles.

**C10 completes session export.** Lossless JSONL retains verified ancestor
suffixes; Markdown is bounded human content; `/export support` writes a bounded
structural trace with only static kinds, seq/time, closed outcomes,
counts/booleans and numeric usage/cache/edit facts. Arbitrary session/provider/
host strings have no output field, and a canary session proves prompts,
answers, reasoning, titles, paths and ids do not serialize. Q18 still owns the
larger preview/runbook bundle.

**POA04 and PAN04 are production-reachable.** Credential-backed composition
now constructs strict `OpenAiProvider` and `AnthropicProvider` routes over the
shared HTTP service; isolated owner-only test references prove activation with
zero requests. OpenAI's buffered compact operation preserves user+opaque output
items and exact continuation. Anthropic merges the current beta edit, preserves
complete assistant state and returns a C12 checkpoint. Portable remains the
default. OpenAI's exact cache option now reaches ordinary Responses requests;
both providers emit neutral detailed facts that survive v2 replay and render in
the inspector. Provider-owned restart Settings default off; root resolves them
before publication, and Anthropic counting consumes the same generation.
Anthropic count evidence is truthfully `Estimated(ProviderTokenizer)`, not
Exact. POA05/PAN05/PAN06 are complete.

**U16/CMD07 neutral inspection is complete.** Default
`status-context` owns `/context` and `/usage`. The first preserves envelope
contributor/refusal evidence, durable window bounds, published-price cost or
Unknown, latest durable detailed cache/edit facts and live compaction rows.
The second projects JSONL token lower bounds/routes/outcomes/cost completeness
with separately bounded turn/response detail. Cache-aware arithmetic requires
a proven non-overlapping partition and every used price; otherwise cost is
Unknown. `/compact list|<strategy> [keep]` uses the registry.

**PL08 managed admission is complete.**
`heycode-extensions` evaluates source/channel/publisher/version/digest/signature/
platform/capability independently before any cache mutation, freezes the exact
authorized bytes, and rehashes/rechecks current policy before declarative host
callbacks. Unverified signatures and upstream checksums remain Unknown/denied.
Production PL06 state operations now require admission for every install,
enable, update and rollback. Missing managed authority denies before mutation;
disable/remove remain available. `ManagedLifecycleAdmission` binds and rechecks
an exact supplied cache/source/catalog/host/policy generation.

**PL07 dependency resolution is complete at its exact library/lifecycle
boundary.** One explicit target platform plus a complete manifest generation
produces deterministic dependency-first order with plugin-id ties. Required
and present-optional SemVer ranges, symmetric conflicts, duplicate identities,
cycles and unsupported platforms have distinct bounded diagnostics. The opaque
graph gates frozen cache publication, one-write lifecycle reconciliation and
ordinary/managed declarative activation before any mutation. Candidate
discovery/version acquisition remains separate; PL03's concrete product host
now consumes only the resulting verified generation.

**P10 provider interception is product-reachable and complete.** Every `llm`
implementation owns global effect-registered strict request/response chains.
Request mutation is limited to durable-header fields; auth preview/final
binding, downstream native-tool routes and C05 reconstruction are independently
checked before transport. Response layers receive normalized events or only a
body-free failure class before telemetry/session/UI; optional default
`provider-telemetry` records closed failure dimensions through local-off or
OTLP. Missing `next`, raw layer errors, cancellation and response refusal all
fail/settle explicitly. Scoped plugins are stably dependency-ordered after
profile resolution, which keeps an opt-in late telemetry provider before this
Consumer without changing independent precedence.

**N05 request transforms are product-reachable and complete.** Optional default
`request-transforms` owns the effect registry and post-`next` P10 application
layer; `request-transforms-openrouter` contributes context compression, file
parsing and response healing with provider-owned requested/effective/effect/
cost evidence. Disabled rows have no cost, enabled rows require explicit
Unknown/free/upstream-token/nonzero-page-price evidence, and account override
keeps effective state Unknown. A missing exact option is inserted, equality is
accepted and conflict is refused before adapter/C05 transport admission. The
deprecated web plugin remains separate from POR05's server tool.

**N06 durable tool usage/cost attribution is complete.** Core validates
positive provider aggregate counts and Unknown/published cost evidence without
call identities. OpenRouter Chat emits the documented `web_search_requests`
aggregate before Usage/Finish; Agent commits `server-tool/usage` only after
terminal success. TEL01 projection and `/usage` keep local, provider-exact and
provider-aggregate rows separate, derive exact success/error/unsettled only
from correlated ids, dedupe invalid repeats and never render Unknown cost as
free. No query, arguments, result body or synthetic call enters the report.

**TEL04 committed metrics are product-reachable and content-free.** Optional
default `telemetry-metrics` seeds lineage/runtime/request correlation from the
session prefix without replaying metrics, then listens on the session-owned
post-commit bus. Provider requests, local/provider-exact/provider-aggregate
tools, portable/native compaction and cache activity emit only closed screened
dimensions. Telemetry schema v2 carries positive aggregate counts through local
counters and OTLP delta sums; v1 reads as one and zero is invalid. Local-off
still contains no exporter.

**Q03/U19 accessible interaction is product-reachable.** A reusable fixed-data
journey harness drives production `AppState` through trust, setup, command,
MCP and provider actions/events and captures exact flat frames. That same
bounded, control-sanitized, duplicate-suppressed projection is the production
screen-reader renderer. It emits no alternate-screen/cursor-control bytes,
automatic `TERM=dumb` selects it, and explicit CLI `--screen-reader` selects it
only for an interactive TUI while surviving every exact-argument recomposition.

**P09 per-operation credentials are product-reachable.** Startup preflight can
still diagnose and fingerprint the configured reference, but the value is
dropped before provider publication. Production DeepSeek, OpenRouter, OpenAI
and Anthropic retain a registry-backed exact handle; each request resolves
once, retries share only that operation's value and the next request sees
rotation. Foreign-route mismatch fails before registry access, and strict
durable auth evidence names each configured custom reference rather than
`AdapterOwned`.

**A05/U18 native composer delivery is product-reachable.** Active native Enter
queues Steer, rebindable Tab queues FollowUp and Esc only interrupts. Text
stays private/durable until Agent's atomic claim publishes `user/message`;
queue counts and state-specific keys render in full and flat modes. One Wake
starts one owned follow-up before queued commands, resume seeds pending work
and a settlement race converts an idle steer into a cancel-recorded follow-up.
Delegated runtime controls remain visibly unavailable rather than targeting
the wrong native session.

**O02/O03 native subagents are complete.** Fresh children are blind durable
sessions, forks are verified shared-prefix lineage, and continuable children
reuse their session. Registry-minted authority binds owner/depth/retention;
nested tools see only owned children, foreign ids look unknown, one-shot
children cannot leave continuable descendants and follow-ups cannot reset
depth. Provider handle shape/id/uniqueness is admitted before publication and
Context remains the terminal lifecycle owner. O07 is now optional/actionable.

**QSEC02 is a passing product-level secret canary.** A deterministic
environment value is positively observed by credential resolution and the
strict Authorization header, while the provider body/system prompt, UI/debug,
request projection, physical JSONL, process diagnostic and redacted support
artifact contain none of it. CredentialProvider failure strings are now
discarded at the registry boundary after a red regression proved arbitrary
provider text could otherwise enter public errors.

**Q04 has one reusable persisted-session oracle.** Fixtures seed ordinary
session inputs, commit the live call's C02 snapshots, flush/reopen physical
JSONL, project the exact request id and pass only through production C05 with
the exact adapter. Chat, Responses, Messages, Gemini and Bedrock share the
matrix. An overwritten persisted envelope byte fails while in-memory state
remains valid, proving the oracle does not self-compare memory.

**All three MCP listing families are live.** `McpListingSupport::CURRENT` now has `tools`, `resources` and `prompts` all `true`, so every zero the MCP panel renders is a zero heycode actually walked to. MCP09 traded robustness for honesty by owner sign-off: a required server advertising a listing heycode cannot walk fails composition, because `McpContributionCounts` is documented complete and cannot express "unknown" (GOTCHAS #181).

**QSEC03 host-local implementation is green but not accepted complete.** Domain block rules now fail closed for literal-address translation and unapproved IDN homographs at every registry/portable policy seam; Seatbelt encodes dynamic roots as string data and the crafted-root native macOS cell passes. Web 48, exec 89 and sandbox 51 tests are green with focused clippy. The row remains active because Linux/Windows runtime cells have not executed natively and Seatbelt's documented pre-existing-hard-link inode alias remains.

**TEL03 is product-reachable but remains opt-in.** New crate `heycode-telemetry-otlp` owns restart-applied wire-safe settings, per-batch credential reference resolution and bounded OTLP/HTTP JSON over the composed HTTP service. Its factory is available to named profiles but absent from the default order; local-off remains structurally unable to emit. Real composition proves mutual exclusion, service ownership, settings/inventory and `/plugins verbose` without network traffic. HTTP/JSON only is supported; no live collector evidence is claimed.

**Cross-platform CI is written but has never run** (`.github/workflows/cross-platform-gates.yml`, Q07, `[~]` not `[x]`). Writing a gate and passing a gate are different claims (GOTCHAS #182). Cross-checking with `cargo check --target` found a real Windows build break in `heycode-extensions` — a let-chain reading `lock.path` where `FileLock` is a unit struct off-unix, dead at runtime and fatal at type-check (GOTCHAS #183). Linux cross-checks pass for every crate tried. Several crates could not be cross-checked at all because `ring` needs a target C compiler this host lacks: `heycode-skills`, `heycode-runtime-claude`, `heycode-runtime-codex` and everything downstream of `heycode-http` are **BLOCKED, not passing**.

**The plugin cache is deliberately unix-only**, failing closed with `PackageCacheError::UnsupportedSecurity` off-unix rather than pretending to work without the inode, nlink and flock guarantees it relies on.

Companion files: [GOTCHAS.md](GOTCHAS.md) (**349** hard-won lessons) and the authoritative [engineering tracker](engineering/TASKS.md). Root owns composition and all central tracker/law/status integration; user-authorized separate Codex tasks may implement disjoint crate slices but never share those central files.

Generated static capability vocabulary/route classes: [provider and model
capability reference](reference/capabilities.md). Live per-model evidence stays
in the catalog and picker.

Current user and implementation paths start at the [guide index](guides/README.md).
DOC04/DOC05 are freshness-gated; DOC02 remains active only because its
fresh-machine/provider evidence is not yet observed.

## One-paragraph state

heycode is a Rust terminal coding-agent foundation (62-crate workspace; 120 default plugins on Unix) with transactional dependency-ordered scoped plugins, canonical trust/profile recomposition, immutable Unix plugin/attachment/retained-output stores, concrete declarative and code-aware extension/MCP activation, exact attribution, strict provider request/response interception plus effect-owned request transforms, typed UI/doctor/command/runtime discovery, five capability panels plus Diff/Jobs/Agents side panels, a product-reachable flat screen-reader shell, CAS-backed settings browsing, persisted routing/native policy, provider-independent bounded web with untrusted labels, capability-gated image/document/audio/rich results, effect-owned LSP tools, durable background execution/subagents/goals/workflows/schedules/teams/reviews, deferred large-catalog selection, durable loop budgets, effect-owned native/portable/prune compaction, JSONL-truth/SQLite-projection sessions, neutral context/usage inspection, committed closed metrics and explicit local-off/OTLP telemetry providers, exact provider continuation state, permission-complete ACP v1, an installed VS Code/app-server bridge, composed OpenCode/DeepSeek Harness process runtimes and fixture-locked Rust/TypeScript clients. **The product is not engineering yet.** Authenticated provider/OpenRouter evidence, OpenCode and Harness live sessions, managed code-plugin authority, Windows/Landlock confinement, hosted continuous fuzzing, security-policy approval and signed/fresh-machine release certification remain open. The complete target and evidence are in [engineering/README.md](engineering/README.md) and its [tracker](engineering/TASKS.md).

## Ship-ready program

The active program adds a plugin-hosted product shell, versioned settings and credentials, authorization flows, live model catalogs, capability-aware provider adapters, delegated Codex/Claude/OpenCode runtimes, provider-native tools and compaction, full MCP, distributable plugins, execution-world seams, persistent terminals/LSP/jobs/workflows, attachments, a provider conformance lab, security hardening and release operations.

Live verification on 2026-08-24 proved Codex subscription, Claude subscription and OpenCode→OpenRouter `stealth/ox-alpha`; Z.ai now documents that alias as the pre-release identity of GLM-5.3-Flash. heycode OpenRouter reached the service but returned HTTP 401 from the stored credential, demonstrating why live validation remains P0. The real-home offline smoke migrated the frozen profile and legacy credential file with backups/modes verified; it did not make a network request or claim that the migrated OpenRouter credential is valid.

Historical OS-store incident (2026-08-25): a dummy OpenRouter entry was written outside the test home. As of the 2026-09-05 home-file-only change, heycode never accesses that store, so the entry cannot shadow its file credential. It is deliberately not read, imported or deleted. This removes the storage blocker, not the separate need for valid authenticated live-provider evidence. See GOTCHAS #326.

Local config note from the 2026-08-25 U11 gate: the first fake-doctor invocation created but did not pass a temporary `HEYCODE_HOME`, so normal startup migrated `/Users/naresh/.heycode/config.toml` from schema 1 to 4 and created a byte-exact `.v1.bak`; no provider request or credential resolution ran. Schema 27 added exact `llm.protocol` and optional provider-owned `llm.max_output_tokens`; schema 28 adds only the Settings-policy dependencies required by an exact selected OpenAI/Anthropic/cloud inference profile. `openai_chat|openai_responses` are canonical while the accidentally derived `open_ai_*` spellings remain read aliases. The authorized 2026-09-05 live launch upgraded the real home from v28 to v29 with `config.toml.v28.bak`, reached the Welcome screen, and reported the stored OpenRouter file credential as unauthorized. No credential values were printed and no OS store was accessed. All gates use an explicit absolute temporary home. See GOTCHAS #78/#88/#90/#93/#94/#96/#102/#104/#107/#115/#118/#119/#121/#123/#132/#133/#196/#225/#233/#237/#259/#260/#282/#289/#294.

## Verification (must pass after ANY change)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # zero tolerance
cargo test --workspace                                   # only meaningful when no lane is mid-edit
HEYCODE_HOME=$(mktemp -d) cargo run -q -- --restricted-workspace --fake run "smoke"   # offline end-to-end
```

Live-provider check (needs a valid provider credential):
`cargo run -- --trust-workspace run "list the files here"` · interactive: `cargo run` (unknown workspaces use the typed U01 modal; missing/invalid credentials use the U10 authorization/recomposition path). OS keychain entries are ignored; live checks still require a valid explicitly selected credential.

## Crate map (dependency arrows point down)

| Crate | Owns | Tests |
|---|---|---|
| `heycode-provider-openai` | strict Responses/state/compaction/cache plus Settings-derived hosted-tool policy and effect-owned N01 rows; upper computer/image/remote-MCP loops remain | 70 |
| `heycode-provider-anthropic` | strict Messages/state/compaction/cache/context/counting plus Settings-derived Search/code/advisor/tool-search/MCP plan and pause replay | 91 |
| `heycode-authorization-aws` | PAWS01 AWS credential chain plus exact saved-region/reference validation and credential-safe hosted canary: each documented profile shape to its own source, static-key pairs all-or-nothing, unreadable config blocks discovery, reports publish no secret values | 51 |
| `heycode-authorization-gcp` | PGCP01 ADC/project/location: project confirmed vs unset vs undetermined kept distinct, ordered location precedence, probe reads only non-secret paths | 76 |
| `heycode-provider-minimax` | PMM01–03/05 products/catalogs/state plus seat-or-Credits eligibility and a canonical-launch operation-credential MCP bundle awaiting root bound factory/live connection | 127 |
| `heycode-provider-zai` | PZA01–04 distinct plans/catalogs/state plus durable native web and canonical Node/package-pinned MCP launch plans; bound factory/clear-thinking/live gaps explicit | 69 |
| `heycode-hooks` | O08 hook service plus O09 structured prompt/subagent/MCP ports and durable provenance/failure policy; concrete product handlers remain | 22 |
| `heycode-telemetry` | TEL02 local-off structural no-egress plus schema-v2 positive-count closed events and lifecycle-owned batching/redaction boundary | 119 |
| `heycode-telemetry-otlp` | TEL03 explicit OTLP/HTTP JSON provider with restart-applied safe settings, per-batch credential resolution, bounded retry/cancellation and closed faults | 15 |
| `heycode-provider-lmstudio` | LM Studio explicit control plus distinct conditionally composed Ollama catalog/profile/inference/inspector, joined picker and five-surface content-withheld live gate; installed Ollama remains absent | 111 |
| `heycode-provider-google` | Developer/Vertex/Claude catalogs, bodyless Vertex setup readiness, Settings-backed activation, lazy GCP readiness, Search/external/code/cache projection and Sonnet 5 wrapper | 149 |
| `heycode-provider-aws` | Bedrock draft-region and selected-region discovery plus Settings-backed Converse, exact dialect factories, independent credentials, selected-model metadata policy and canaries | 149 |
| `heycode-provider-azure` | exact Azure OpenAI v1 resource/deployment readiness plus operation-credentialed Responses inference and conditional production composition | 12 |
| `heycode-provider-openai-compatible` | validated version-root URL, bounded canonical model discovery, explicit model fallback, optional operation credential and strict selected-model Chat inference | 13 |
| `heycode-install` | Q14/Q15 authenticated artifacts, candidate-owned compact-dist install, exact size evidence/stable-path smoke, enabled-plugin API policy and directional rollback; hosted attestation evidence remains | 67 |
| `heycode-live-artifact` | Q08/QLIVE01 live-test artifact schema: route/outcome/instant/latency metadata, closed failure and skip classes, content withheld unless explicitly opted in, credential screening and future/expired freshness rejection | 17 |
| `heycode-core` | Context/Plugin/scoped compose, exact inventory/events/seams/ids plus validated detailed/untrusted/rich/provider-state and audio metadata vocabulary | 73 |
| `heycode-sdk` | app-server v1 wire vocabulary and typed Rust client, including metadata-only assistant audio, runtime/workspace selection and bounded correlation | 6 |
| `heycode-extensions` | strict manifest/cache/lifecycle/managed admission generation plus typed code runtime/entrypoint metadata, PL09 protocol and PL10 WIT/Component boundary | 140 |
| `heycode-extension-host` | concrete PL03/PL04 adapters plus generation-bound installed code authority, six adapters, exact-byte process host and import-exact Wasmtime engine | 24 |
| `heycode-doctor` | effect-owned async check registry, validated ids/codes, static safe text, closed typed evidence, cancellation/panic containment, schema-v1 JSON/human report and K08 check plugin | 3 |
| `heycode-ui` | validated panel/dialog/status/side-panel descriptors, typed opaque handles, slot/priority/id snapshots, attributed dynamic inventory and token-checked disposal, effect-owned settings UI, terminal themes, persisted keymap and UI preferences | 55 |
| `heycode-http` | plugin-owned reqwest/rustls HTTP/SSE/buffered/WebSocket-fallback plus pull-owned dynamic responses, cancellation/reconnect, bounded backpressure/bodies and URL/body-free errors | 47 |
| `heycode-exec` | capability-rooted filesystem and mandatory sandbox/subprocess/shell plus exact env/argv/process trees, retained output/LSP/terminal ownership and executable-image leases | 89 |
| `heycode-session` | v2/v1 migration, caller-minted creation, hook/runtime/attachment/document/audio/request/provider-state/inbox/server-tool plus operational lineage/export/repair | 190 |
| `heycode-attachments` | owner-only immutable SHA-256 objects, MIME/image/WAV/size admission, path race checks, read verification and durable metadata commit | 6 |
| `heycode-trust` | canonical workspace identity, typed trust/restricted/unknown gates, live prompt actions and Unix owner-only cross-process-CAS persistence | 21 |
| `heycode-prompt` | ordered sections registry (+ late/shared registration), PLAN MODE section | 4 |
| `heycode-llm` | exact-route adapters/catalogs/retry/counting/interception/transforms/compaction plus no-cache endpoint/coordinate probes, explicit unauthenticated Chat binding, joined provider readiness, hidden audio and exact cloud/hosted state | 432 |
| `heycode-catalog-file` | schema-v2 owner-only catalog cache with v1 safe degradation plus layered field-attributed user/project overrides that never mutate provider evidence; CAT07 product display remains | 26 |
| `heycode-native-tools` | effect-owned provider/client/MCP registry, durable route vocabulary and live Settings-backed prefer/only policy with fail-closed selection | 6 |
| `heycode-web` | provider-independent search/fetch/processor registry, live provider/domain policy with literal/IDN block-list hardening, DNS-pinned portable transport and effect-owned bounded local/web HTML/PDF/text extraction with durable sources | 48 |
| `heycode-provider-deepseek` | operation-time V4 discovery, strict optional features plus schema-selectable guarded Anthropic adapter and rotation proof; credentialed smoke remains | 80 |
| `heycode-provider-openrouter` | auth/catalog/routing/transforms plus web citations, aggregate usage and a combined content-withheld CI conformance artifact without invented calls | 50 |
| `heycode-tools` | Tool trait with plain/rich output and guarded pipeline; filesystem/shell/todo/web plus LSP server/definition/references/diagnostics Consumers, retained-output spill, native routes and token-owned MCP registrations | 81 |
| `heycode-runtime` | validated native/delegated descriptors/ids/account state, text/media RuntimeInput, tri-state capabilities, cancellable lifecycle, strict raw-event normalization plus generic bounded ACP/OpenCode framing/catalog/model/permission/process ownership | 26 |
| `heycode-runtime-opencode` | pinned OpenCode 1.18.21 executable/hash/version/agent identity, explicit environment, common raw-process ownership and official GLM content-withheld live gate; installed catalog lacks the route | 9 |
| `heycode-runtime-deepseek-harness` | pinned Harness SDK v0.0.1 request/notification union, launcher/reviewed-artifact hashes, normalized lifecycle/tool/usage events and whole-process cancellation; runnable clean local artifact remains | 9 |
| `heycode-agent` | reusable cancellation/native runtime plus provider readiness, shared approval, lifecycle-hook ports, authority-scoped subagents and durable operational domains | 232 |
| `heycode-app-server` | stable local JSON-RPC v1 backend/transports plus metadata-only audio and effect-owned auth/provider/runtime/MCP/plugin/settings controls | 17 |
| `heycode-init` | bounded manifest discovery, managed-section diff, opaque stale-state token, atomic mode-preserving apply and effect-owned `/init` | 5 |
| `heycode-status` | effect-owned effective status/web plus evidence-preserving context/usage/cache/edit commands, bounded owner-only health history and `/health` | 27 |
| `heycode-routing` | routing settings schema/selection/CAS service, provider/model/coordinate validation, atomic pending connections, reload apply, split routing/auth commands | 12 |
| `heycode-authorization` | safe flow catalog, effect registry, pre/post cancellation, registry-owned credential commit + authoritative readback receipts | 3 |
| `heycode-authorization-api-key` | event-driven secret broker/service, correlated masked prompts with drop revocation, OpenRouter/DeepSeek validation, stable taxonomy and official probe | 6 |
| `heycode-config` | schema-v30 home-file credential migration and exact protocol/policy dependencies plus profile-v3 scopes/worktree/managed code authority, every-version matrix and S13 previews | 84 |
| `heycode-credentials` | validated refs/kinds/provider ids, safe descriptors, zeroizing secrets, precedence/effects, body-free provider failures, private fingerprinted TTL validation + rotation invalidation and secret-blind doctor check | 12 |
| `heycode-credentials-command` | precedence-5 exact-argv provider, synchronous worker over common subprocess/sandbox, tree timeout, env/output bounds, one-line normalization and fixed failures | 4 |
| `heycode-credentials-env` | precedence-0 process environment inspection/resolution, blank-is-absent, read-only shadow behavior, map-backed test reader | 2 |
| `heycode-credentials-file` | precedence-20 schema-v1 owner-only fallback, operation-time rotation, atomic exact-backup legacy migration, conflict/symlink safety | 4 |
| `heycode-settings` | schema/validators, explicit fail-closed wire exposure, layered immutable snapshots, expected-revision CAS, generation publication, ordered/panic-contained/effect-owned watchers and doctor check | 9 |
| `heycode-settings-file` | schema-v1 TOML, atomic `0600` writes, format preservation, trusted-project read layer, notify 8.2 canonical watch/reload, last-good invalid reload | 5 |
| `heycode-tui` | correlated provider/native/audio renderers, cloud/endpoint onboarding, 100K-event transcript, MCP elicitation/progress/log bridge, human commands and bounded full/flat panels | 234 |
| `heycode-sandbox` | Seatbelt/bwrap/Landlock filesystem Providers with truthful capability facts, injection-safe Seatbelt root literals, native macOS attack matrix, Linux ABI/runtime workflow and Windows unsupported/Job Object certification lane | 51 |
| `heycode-mcp` | registry/state, stdio/dynamic HTTP/OAuth, atomic listings/results/management, duplex product routing, shared approval and provider-bound launch builders | 358 |
| `heycode-onboarding` | Welcome/runtime/method/connection/model dynamic snapshots/actions, ordered cloud-coordinate forms and recomposition outcomes, blocking state service/plugin | 17 |
| `heycode-skills` | capability-bound, component-wise no-follow SKILL.md discovery with immutable snapshots; load_skill/catalog prompt plus effect-owned `/skills` and `/skill` | 13 |
| `heycode-runtime-claude` | canonical installed-CLI identity, compatible version/auth status, tool-free no-persistence handshake plus current SDK-control stream-json sessions with host identity, resume/fork, partial events, follow-up, compaction and permission/question callbacks; R08 ephemeral restrictions pass installed 2.1.251 | 32 |
| `heycode-runtime-codex` | exact 0.146.0 app-server launcher/binding including safe shim/interpreter identity, raw strict JSONL, credential-blind account/catalog, primary and ephemeral start/resume/fork/steer/compact sessions, correlated permission/question callbacks, cancellation and truthful containment | 43 |
| `heycode-cli` (bin) | trust-first production graph with 120 Unix defaults, schema-v29 policy routing, session-scoped MCP/hook/TUI attachment, Bedrock/Vertex/Azure onboarding and exact recomposition | 211 |

## Service-key registry (ctx map)

`doctor` · `trust` · `ui` · `settings-ui` · `settings` · `http` · `sandbox` · `subprocess` · `terminal` · `shell` · `hooks` · `filesystem` · `retained-output` · `lsp` · `credentials` · `authorization` · `secret-prompt` · `aws-auth` · `gcp-auth` · `lmstudio` · `lmstudio/model-control` · `lmstudio/models` · `ollama` · `ollama/catalog` · `ollama/profile` · `ollama/inference` · `onboarding` · `profiles` · `session` · `attachments` · `session-query` · `prompt` · `native-tools` · `web` · `document-extractor` · `providers` · `llm` · `provider-interception` · `request-transforms` · `models` · `catalog-overrides` · `runtimes` · `mcp` · `mcp-management` · `plugin-lifecycle` · `telemetry` · `tools` · `seam/pre_tool` · `approval` · `approval-interactive` · `commands` · `health-history` · `routing` · `agent-options` · `compactions` · `agent` · `questions` · `app-server` · `plan` · `subagents` · `jobs` · `execution-jobs` · `goals` · `workflows` · `schedules` · `teams` · `reviews` · `token-counters` · `skills` · `release-manager` · `tui`. The exact **71-key** registry and applicable default subset are composition-tested; sandbox mode off still owns the mandatory policy path with an available-but-inactive backend when supported. Retained-output/lsp-tools default only where the audited retained-output backend exists (currently Unix); Ollama services appear only for explicit Ollama selection.

## Session event kinds (v2 current set; v1 gate frozen)

**session/created · runtime/linked · runtime/configured · hook/contribution · agent/inbox/splice · goal/change · workflow/change · schedule/change · team/change · review/change · request/header · request/context · user/attachments · attachment/added · assistant/audio · assistant/provider-item · assistant/response-metadata · server-tool/call · server-tool/result · server-tool/usage · assistant/citation · tool/rich-result · compaction/native** (v2-only) · turn/start · turn/end{stop|max_tokens|max_steps|max_elapsed|max_tool_calls|unreported_token_usage|clock_unavailable|error|aborted} · step/start|end · user/message · assistant/chunk (ignorable) · assistant/message · tool/call · tool/result · compaction/applied · plan/mode · session/title. The generated [session-event reference](reference/session-events.md) is the freshness gate. `assistant/audio` retains metadata only; bytes stay in ATT01. `hook/contribution` reaches model projection only after append/flush/readback. V2 plain/rich results may carry typed Web/MCP/LSP provenance; v1 cannot claim those fields/kinds.

## Foundation deferrals and engineering gaps

1. **Integrated onboarding and product shell** — trust-first modal/recomposition, first-run validated credential commit/readback/recomposition, command/model/runtime/permission/settings/session and MCP/plugin/skills/agents/hooks panels ship. Provider defaults plus the effective permission are sufficient for the first composer; Settings-backed hot permission changes and Windows persistent trust remain open.
2. **Provider kernel** — DeepSeek/OpenRouter plus OpenAI/Anthropic/Google/AWS/Azure product routes use catalog-backed verified dispatch. Provider-native tool policies are request-specific; portable web remains DNS-pinned and untrusted-labelled. ATT02–ATT04 add exact image/document/hidden-audio projection after capability proof. Authenticated provider, external grounding/cache policy and several upper action/MCP bridges remain active.
3. **MCP and plugins** — registry-backed stdio/Streamable HTTP, atomic listings/results, OAuth, management, official conformance, exact Agent/TUI elicitation routing and annotation-independent shared approval ship. Bound MiniMax/Z.AI generations and concrete O09 handler providers remain open. Declarative/bundled-MCP plus native/WASI code activation are concrete, while managed code grants/resources and publisher verification remain open.
4. **Deep agent capabilities** — capability-rooted filesystem/process/raw-IO/shell/terminal/LSP/job/hook services, strict delegated-event normalization, durable inbox/request/state projection, queryable shared-prefix sessions, attachment projection, shell/PTY background execution, goals, plan commit, workflows, schedules, native/Codex/Claude subagents, permission-complete ACP, composed OpenCode/DeepSeek Harness process plugins, stable local app-server/TUI controls and Rust/TypeScript SDK packages ship. Authenticated external host/IDE proof and the two new runtime live turns remain open.
5. **Release hardening** — macOS Seatbelt and real Linux Docker bwrap evidence plus artifact/update/rollback/channel/API boundaries, GitHub verifier/release manager and candidate-owned install/stable-path smoke ship. Hosted Landlock/Windows, genuine signed release transactions, provider matrix, security approval and three-platform fresh-machine turns remain open.
6. **Native sandbox certification** — Landlock UAPI/ABI logic cross-compiles and bwrap/fallback pass real Linux Docker, but Docker Desktop lacks Landlock; the pinned Ubuntu workflow must still prove the strict Landlock matrix. Windows cross-compilation proves Job Object cleanup code and unsupported restrictive choices, not native execution or filesystem denial. E11/E13 remain active (GOTCHAS #97).
7. **ACP/IDE proof** — the scripted exact-id allow/deny/cancel/wrong/duplicate/late tool-permission loop and an installed VSIX over the shipping stdio app-server both pass. External remote-host/listener authentication remains outside this local child-process boundary.
8. **Delegated-runtime depth** — Codex and Claude primary bridges plus fresh one-shot delegated subagents, generic ACP interoperability, and heycode-exec-backed OpenCode/DeepSeek Harness plugins ship at their accepted scopes. R10 still requires a trustworthy authenticated OpenCode GLM turn; R12 requires a runnable local Harness SDK artifact and observed turn.

## Gotcha index → [GOTCHAS.md](GOTCHAS.md)

Read it before touching: delegated pinned-protocol closure (#136), inbox wake rules and atomic claims (#137), subagent provider contracts/authority (#138/#246/#259/#260), open delegated unions and shipped-binary evidence (#139), live-evidence discipline (#261), compaction/provider/inspector/session-lifecycle/plugin-policy/export/interception/transform/usage/metric/accessibility/credential/composer/security/replay/release/docs evidence scope (#145/#225–#258), session/waterfall/config/runtime fundamentals, composition/profile boundaries (#62–#68), settings/credentials/auth (#29–#40), catalogs/inference/state (#41–#60), UI/command/runtime/process continuity (#69–#95), filesystem/provider/native-sandbox/inbox/protocol/package boundaries (#96–#103), trust/capability paths/raw IO/retry/delegated runtimes/lineage/MCP/install cache (#104–#112), first-run/provider/native/web/attachment/extraction/protocol/SDK/runtime controls (#113–#135), and docs (#61).
Mistral connection binding: catalog regression passes for chat-only admission, archived exclusion, context size and independent tool/vision evidence; shared raw-SSE fragmentation tests pass for all three compatible providers. Full expansion verification remains pending. No new service or event kind.

Together and xAI use separate catalog dialects (chat task array versus language-model envelope), with exact documented tool evidence and independent modality evidence. Seven catalog regressions and shared SSE conformance pass; expanded real-adapter composition checks pass for Fireworks, Groq, Mistral, Together, xAI and LM Studio.

LM Studio optional endpoint-key setup is wired through masked entry, explicit provider capability gating, isolated validation, authoritative credential commit and endpoint/model/reference staging. Focused tests pass for one-operation authorization (4), endpoint credential isolation, wizard key choice and TUI validation failure preserving the prior key. Startup transaction verification exposed missing Settings public-role declarations for credential references; these are now explicitly declared and the authenticated/unauthenticated restart transaction test passes. No new service or session kind.

The real full-screen PTY optional-key scenario passes against an isolated HTTP fixture: masked input, authenticated catalog choice, credential-file commit and no secret in terminal output. This is UI/integration evidence, not a live LM Studio inference claim. Targeted all-target clippy passes for CLI, TUI, routing, authorization and local-provider crates.

Ollama optional key setup now covers native discovery and exact Chat credentials. Twelve focused Ollama tests pass; expanded real-composition credential checks pass for both local providers and all five compatible providers. A compiling mutation that dropped the saved local credential reference on startup was killed by the restart regression, followed by exact source restoration and a passing rerun. The real full-screen LM Studio-shaped fixture key scenario also passes.

Gemini onboarding now limits model choices to the provider-owned production adapter scope. A real-composition regression first reproduced catalog-only model staging, then passed with admission enforced before persistence. Workspace fmt/clippy and all 3,990 tests pass after this correction; documentation verification passes. No new service or session kind.

Cloud coordinate persistence/factory wiring is implemented. Routing retains bounded non-secret coordinates through Settings/restart/model/runtime changes. AWS validation and status share the saved region with inference/catalog admission and report its connection origin. Vertex now has a reachable project/location form, a credential-blind maintained refresh and an uncached readiness path that independently requires ADC and the configured operation-time OAuth token before one bodyless model-config GET. A production composition restores the same coordinate/model/reference tuple into the catalog and lazy inference route without changing process environment. Affected all-target Clippy and 17 onboarding, 149 provider-Google, 234 TUI and 198 CLI tests pass, including both managed-cloud production PTY paths. A compiling mutation that promoted undetermined ADC state was killed and the restored regression passed. No live cloud account was used and no new service or session kind was added.

User scope update: the isolated cloud/custom-server task is complete. Amazon Bedrock, Google Vertex AI, Microsoft Azure OpenAI and one exact custom OpenAI-compatible Chat route are production-reachable through provider-owned forms with fixture-backed readiness. The branch changes have been applied to the primary working tree after verifying every affected file against the branch baseline. Integration review restored the three-entry contract and repaired Azure/custom normal Agent tool admission. Full read-tool and rejection regressions exercise the real composed world; capability evidence remains Unknown.


The permission menu has three choices: Full access, Accepted edits and Default.
Auto is removed from the menu and command catalog. Accepted edits remembers only
explicit Accept all future edits decisions for the exact tool and complete JSON
inputs in this conversation. Accept is one-time; changed parameters ask again.
Default bypasses remembered grants. Concurrent identical requests share only an
explicit future grant; cancellation and rejection never grant future access.
Schema 30 still preserves legacy unconditional `auto` configuration as
`full_access` without rewriting explicit files.

Provider credential setup now validates saved API keys through their registered
provider-owned validator before model discovery. Public catalogs do not establish
authentication, and failed or stale remote discovery no longer advances to default
model selection. Rejected newly entered keys reopen an empty field with a retry
error before persistence. The field has no placeholder bullets, shows first five
and final characters for sufficiently long keys, and masks short keys completely.
