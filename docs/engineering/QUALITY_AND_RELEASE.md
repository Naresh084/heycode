# Quality, security and release plan

## Quality doctrine

No capability is “supported” because a unit test passes or an adapter can produce text. Product claims require contract, composition, replay, UI and live evidence appropriate to the capability.

Every behavior change starts with a failing test that proves the user-visible or architectural contract. Tests describe expected behavior rather than preserving an obsolete implementation.

Handled agent failures are additionally checked against C08's repair
projection: a provider/tool error path is complete only when no turn, step or
call remains open. Failure UI is evidence only after those durable records
commit. A torn final JSONL append is storage evidence and remains a retryable
reader error, never a synthetic repair record.

Context/token tests must construct envelopes through the production request
planes. Hand-assembling `TokenEnvelope` is suitable for arithmetic unit tests,
not evidence that prompt, tools, provider state and attachments are reachable.
An exact counter may measure transcript messages only when it can encode their
complete structured form; otherwise its refusal remains visible and a lower
evidence counter may win. Media and unknown state never default to zero.

Retained-health tests exercise both caps independently, reopen through a second
store instance, preserve file order across backwards clocks, distinguish a torn
tail from a corrupt terminated row, and refuse newer/foreign files without
overwriting them. Product acceptance additionally requires the default
composition to expose the service and `/health`; a crate-only store is not a
reachable diagnostic.

Profile parity requires two production Consumers. A unit test that calls one
store twice cannot prove CLI/picker agreement. K07 acceptance traverses the
effect-owned profile service and queued command into a typed TUI selection, then
the CLI reloads the same strict layer through startup while preserving every
other argument. Historical exact TUI profiles receive only their two new
prerequisites through schema v21. Schema v22 independently inserts
`compactions` before the first Agent/subagent Consumer and leaves profiles with
neither untouched.

Compaction acceptance is transactional and route-aware, not “a summary string
appeared.” Tests make a strategy mutate during preparation, propose a nonexistent
prefix and return the wrong replacement class; all must fail before the
registry-owned append. Native success must traverse resolved call → optional
provider operation → bounded checkpoint → v2 reopen → exact-route projection.
Neutral/incompatible routes must retain original history, v1 must reject the
kind, and a marker cannot shadow itself/future seqs. `/compact`, pressure and
native RuntimeSession tests must observe the same registered portable row.

C14 tests all three effects through production routing. The initial direct
selection must fail with unchanged Settings/session; cancel keeps both exact;
fork adds one child at the pre-checkpoint event count while keeping the parent
and route exact; portable adds one settlement before the Settings revision/live
selection changes. An equal-boundary portable marker must win because it is
later. App-server mapping is Conflict, and errors/checkpoint content remain
body-free.

Provider-native acceptance joins three independent proofs: provider transport
fixtures, the shared C12 Agent transaction fixture and a production-loader
composition with the selected provider. The last uses a unique owner-only
temporary credential reference, disables the normal fake explicitly, inspects
the strict/native interfaces and performs no request. A provider-local cache or
context-edit metadata ledger is not restart/UI evidence; those rows stay active.
Token evidence follows provider wording: a documented estimate is
`ProviderTokenizer`, not Exact merely because a remote endpoint returned it.

Inspector snapshots must test the negative claims too: no request envelope,
unknown window, unpriced model, unreported step and absent durable cache detail
must each render differently from zero. Usage detail is bounded to 20 turns,
and a strategy listing must write no session/model event. U16 cannot complete
while provider-local cache/edit ledgers are the only source of those facts.

Reload lifecycle tests include terminal registry destruction. A swapped world
may stay open while a reader holds it, but its last strong release must call
`Context::shutdown()` and withdraw every effect exactly once. Q06's generated
model checks live generation, held readers, parked worlds, candidate rejection,
seam layers and disposal together; dropping memory without running disposers is
never counted as teardown.

Route-control acceptance separates durable commit from live publication.
Settings CAS must succeed before a new runtime backend appears; active turns and
opened/differently linked sessions conflict. Workspace tests canonicalize and
contain authority-bearing paths, and product-loader/SDK evidence must traverse
the registered methods before their capability flags become true.

Rich-result acceptance must traverse parser → registered Tool → ordered Agent
commit → ATT01/session → provider/UI/runtime. Parser-only types and display-text
markers are not preservation. Tests require byte commit before the closed
`tool/rich-result`, exact block order, explicit JSON-null presence, remote error
state, MCP untrusted provenance and V1 refusal. Raw media/URIs/server text must
remain absent from Debug, and stale string-result tests are repurposed to assert
the typed projection rather than deleted.

Telemetry-default claims are verified from dependency/source structure: the
local-off crate cannot name an exporter transport. OTLP lives in a separate
opt-in provider, resolves credential references per batch and returns only
closed faults. Completion additionally requires a production profile to swap
the service, expose restart-applied safe settings and show exact inventory;
crate-local exporter tests do not prove reachability. Live collector behavior
remains separate release evidence.

Committed-metric tests subscribe through the injected session service, not the
unrelated Context bus. A reopened prefix seeds lineage/runtime/request
correlation without emitting historical metrics; only a new durably appended
event changes counts. Fixtures distinguish local, provider-exact and provider-
aggregate tools, preserve aggregate count through local and OTLP providers and
prove prompts/arguments/results/paths/bodies have no outbound field.

Accessible-shell acceptance joins renderer and product reachability. Exact
snapshots cover trust, first-run setup, command discovery, MCP management and
provider/runtime selection by driving production `AppState` with typed actions
and events. Flat output must be bounded, control-sanitized, deduplicated and
free of alternate-screen/cursor-control bytes. A CLI contract proves explicit
`--screen-reader` selects that path only for interactive TUI mode and survives
all recomposition argument rewrites; `TERM=dumb` covers automatic selection.

Steering/follow-up tests separate key routing, durable admission and wake
ownership. Active native Enter/Tab/Esc must produce Steer/FollowUp/interrupt
without publishing composer text. Agent fixtures prove next-step re-drain,
atomic claim/UserEcho ordering and one-message-per-follow-up turn. TUI fixtures
prove exact queue counts, state-specific full/flat hints, one-shot wake
consumption, resume seeding, owned follow-up settlement and the UI-liveness race
conversion. A delegated runtime case must retain input and perform zero native
inbox mutation.

Subagent acceptance tests the two axes independently. Fresh must be blind and
durable, fork must verify shared-prefix lineage without copied prompt/history,
and continuable must add later turns to the same child session. Authority tests
mint two owners in one registry, prove foreign list/send/interrupt behaves as
unknown, reject unbound requests before provider execution, prevent one-shot
retained descendants and carry depth through a later follow-up. Provider
contract fixtures reject wrong handle presence/id and duplicates before a row
can become live; shutdown leaves zero live handles.

Secret-canary acceptance requires positive premises, not a repository-wide
negative search. The environment provider must resolve the canary and the mock
transport must observe it in the exact Authorization header. A process spec
must actually contain it before its Debug output is checked. The same journey
then checks request body/system prompt, UI/debug, request projection, physical
JSONL and committed support export. A deliberately leaky CredentialProvider
failure must fail red first and public error/Debug must retain provider/reference
ids without the free-form body.

Replay-oracle acceptance requires persisted independence. Each protocol fixture
seeds its input events, resolves one live call, then the shared Agent helper
commits C02, flushes/reopens JSONL, selects the exact id and invokes C05 with the
same adapter. A physical-byte corruption control must fail while the writer's
in-memory event vector remains valid, and the failure must not echo system/input
content. An appended tail that the live file cursor overwrites is not a valid
mutation kill.

## Verification tiers

### Tier A — static and unit

- Rustfmt.
- Clippy `-D warnings` across all targets.
- Public documentation and missing-doc checks.
- Dependency and feature checks.
- Pure parsers, schema validators, request resolvers, projectors and state machines.
- No network, real home directory or wall-clock dependence.

### Tier B — component contracts

- Mock HTTP/SSE/WebSocket servers with recorded provider fixtures.
- MCP mock servers for protocol/auth/reconnect.
- Fake credential/authorization providers.
- PTY/subprocess/sandbox component tests.
- Provider error and fragmentation matrices.
- Plugin lifecycle and disposal.

### Tier C — real composition

Boot the actual profile through the same factory/loader path as the binary. Mock only external services. Assert durable events, model-visible requests and UI output.

Required composition scenarios:

- First run with no config.
- Legacy config migration.
- API provider setup and model selection.
- Subscription runtime setup.
- Invalid/rotated credential repair.
- MCP stdio and HTTP/OAuth.
- Plugin install/enable/disable/update rollback.
- Native and delegated turns.
- Plan, permissions, jobs, subagents and compaction.
- Graceful shutdown reaping children.

Latest accepted deterministic milestone (2026-08-25): rustfmt and all-target
workspace clippy pass; the POR01/POR02 milestone has 825 tests/53 consolidated
non-empty suites green. The safe unauthenticated official OpenRouter catalog
canary, isolated doctor and fake headless smoke pass. POR03 raises inventory to
829/53; all 287 tests in core/session/LLM/agent pass with targeted clippy. POR04
implementation raises inventory to 835/53 and adds deterministic production-loader
strict dispatch; its full gate, schema-13 doctor and fake headless smoke pass,
while authenticated live evidence remains. U10's isolated
production-loader journey commits a validated custom credential through the
real file provider, performs authoritative readback, tears down and recomposes
a credential-backed world with inactive onboarding, the provider-owned default
model and effective sandbox choice. TUI error/exit paths cancel and join every
owned loop task. Isolated real-binary doctor is 4/4 healthy; fake headless turn
settles `stop`; production-loader DeepSeek mock retries 503, cancels Retry-After
and refuses replay after output; installed Codex/Claude safe handshake canaries
pass. This is milestone evidence, not public-beta certification: Q16 still owns
fresh-machine real-provider onboarding, alongside hosted Landlock, native
Windows confinement/owner security and the Tier-1 live provider matrix.

N01 raises current source inventory to 839 tests/54 consolidated suites and
schema 14. Its 501-test touched-crate batch, targeted all-target clippy and
format check pass. Production-loader coverage proves the exact client web route
choices survive registry resolution, request resolution, durable commit,
projection and C05 verification. The last full workspace/product-smoke milestone
remains the 835/53 POR04 gate; the next full gate is reserved for the next
coherent native-tool milestone rather than being misreported as already run.

N02 raises source inventory to 848/54. Its 300-test core/session/LLM/agent
batch, targeted all-target clippy and format check pass. Fixtures cover
redacted boundary validation, v1 refusal/v2 round-trip, request/step/route and
cross-request call/result correlation, unsafe citations, exact Anthropic pause
replay, terminal-only group publication, preappend structural admission and the
eight-continuation safety cap. The combined N01/N02 milestone also passes full
workspace clippy and all 848 tests, plus isolated schema-14 doctor 4/4 and fake
headless `[done:stop]`.

POR05's deterministic schema-15 slice raises source inventory to 852/54. The
332 affected tests and targeted all-target clippy pass. Production composition
proves provider candidate precedence, client duplicate removal, durable native
feature/tool provenance, exact bounded server-tool request/budget, retry veto,
official nested URL citation validation, exact Chat-state preservation and N02
session projection. The row remains active: this is not authenticated provider
evidence, and the official Chat surface supplies only aggregate search count,
not per-call identity/query.

N04 raises source inventory to 855/54. Six native-tool tests cover all four
modes, client-before-MCP local ranking, provider ownership, unknown overrides,
unavailable-only failures, invalid persisted settings and effect disposal.
Production composition performs live Settings writes from provider-preferred to
client-preferred, proves the next durable header/wire changes coherently, then
sets an unavailable `native-only` fetch and proves refusal before a third
transport call. Affected all-target clippy and focused suites pass. The optional
policy plugin changes no root config schema and intentional exact profiles keep
the original prefer-native registry default.

WEB01/N03 add six tests in a new consolidated `heycode-web` suite, two tool
Consumer/source-law tests and one schema-v16 migration test. WEB02 adds four
authority/lifecycle tests, raising inventory to 868/55. The 290-test initial
affected gate and targeted all-target clippy pass; WEB02's format, warnings-denied
web clippy and all 10 web tests also pass.
Fixtures prove registry dispatch/lifecycle/cancellation, boundary/redaction,
provider-independent Consumers, DDG/Brave wire equivalence, typed IPv4/IPv6/DNS
private admission, bounded HTML normalization, exact `web_provider` inventory
and production composition. The WEB02 matrix additionally proves mixed-answer
denial, DNS pinning, direct/redirected metadata refusal, mapped IPv6, unsafe
location/loop/hop refusal, credential-bearing search redirect refusal and the
absence of a detachable DNS task handle.

WEB04 adds four web-policy tests and one status-plugin test, raising source
inventory to 873/55. Fixtures prove configured search/fetch divergence,
unique-auto and multi-provider ambiguity, dynamic local availability, invalid
persisted-id composition failure, post-commit Settings replacement, bounded
canonical suffix-safe domains, search filtering, pre-dispatch fetch denial,
provider final-URL refusal, redirect-hop policy propagation, exact inventory,
production file persistence and `/web` rendering/disposal. Formatting and
warnings-denied clippy pass for web/tools/status/config/CLI. The affected test
inventory is 198 green after the exact Settings namespace audit was updated from
three to four and that single test was rechecked; the last full workspace gate
remains 848/54.

ATT01 adds two core vocabulary tests, two session event/projection/version tests,
four attachment-store tests and one production-composition test, raising source
inventory to 882/56 across 40 crates. The 280-test affected gate covers exact
content-id/MIME/name/dimension validation and serde redaction; v2 round-trip and
v1 refusal; bytes-before-event reentrant read; duplicate/concurrent no-clobber;
size/MIME/image/cancellation refusal with zero events; owner modes, symlink root,
tamper detection, held-service shutdown; complete factory/plugin/service
inventory and isolated real composition. Formatting and warnings-denied clippy
pass for core/session/attachments/agent/config/CLI. One handcrafted v1 fixture
initially targeted the unsafe temp-root path; after moving it under a valid
session directory, that single test passed without rerunning the other 279.
Native macOS tests plus Linux and Windows cross-compilation pass; Windows still
returns `UnsupportedSecurity`, which is compile evidence rather than native
storage support. The last full workspace milestone remains 848/54.

WEB03 adds four extraction/processor tests, raising source inventory to 886/56.
The 326-test affected gate covers processor uniqueness/ambiguity/effect disposal;
redacted raw inputs; HTML title/body/link extraction with script suppression;
UTF-8-safe caps; valid PDF text/page markers/page count; malformed, encrypted
shape, raw truncation, >2 MiB page content and pre-cancel refusal with zero
attachment events; durable URL/title/time/truncation/page/content-id equality;
escaped tool citations; complete default factory/inventory and production
extraction. Formatting and warnings-denied clippy pass across core/session/
attachments/web/tools/status/config/CLI. Native macOS passes. Additional Linux
and Windows web cross-checks are not evidence: `ring` stopped in its build script
because this host has neither target C compiler, before heycode code compiled. The
last full workspace milestone remains 848/54.

ATT02–ATT03, WEB05 and X03–X06 raise current source inventory to **932 tests / 57
consolidated suites** without changing that last full-milestone claim. Their
focused warnings-denied gates cover exact image/PDF wire, native/extracted
durable routes, untrusted Web request/UI projection and ACP schema content,
tool/plan/usage mapping, hanging-provider cancellation/EOF settlement, stable
app-server JSON round-trips, current schema migration and a production-composed
local-client turn/TUI projection. These offline smokes are not delegated-runtime
or live-provider evidence.

X05's affected warnings-denied gate covers the settings/auth/credential/native-
policy/web/routing/app-server/config/CLI boundary. The four new tests prove
wire exposure is opt-in, dropping a masked prompt future revokes late-answer
authority, correlated authorization commits before its secret-free receipt,
and the production-composed client lists/mutates every control family with
bounded settlement. The first combined run exposed a locked native-keychain
status probe and then an unproven fake-provider model; both were diagnosed and
changed before exact rechecks. Final authorization listing performs no backend
inspection, and model fallback keeps current/default evidence distinct. Across
the initial no-fail-fast results plus only the diagnosed exact reruns, all **193
affected tests** are green. This is not a full workspace, real-keychain, live-
provider or external socket/SDK-host gate.

X06 adds three Rust SDK tests in a new consolidated suite and three TypeScript
tests. They cover typed start/expected-id resume/stream/concurrent cancel,
response/session/sequence refusal, every closed event plus auth/settings casing
through a shared cross-language fixture, and control runtime validation. The
Rust and TypeScript examples both run to cancelled settlement. Formatting and
warnings-denied clippy pass for SDK/app-server/TUI/CLI; all 9 affected Rust
tests and both production-composed app-server smokes pass. TypeScript 7.0.2 is
project-pinned with a lockfile; its strict build, 3 tests and Node-native typed
example pass. This is transport/client evidence only: no external listener,
same-user authentication, IDE host, full workspace or provider call ran.

R04 raises source inventory to **937 tests / 57 consolidated Rust suites**.
Warnings-denied runtime/Codex clippy and all **50 affected tests** pass. Five
new real-process fixtures prove plan-only credential-blind account projection,
Disconnected versus NotRequired, two-page model/capability normalization,
body-free malformed/loop refusal and cancellation-before-process-settlement.
The opt-in installed Codex 0.146.0 canary separately calls only
`account/read {refreshToken:false}`, provider capabilities and model metadata;
it passes without printing account labels or reading token/auth files in heycode.
This is not login/logout, rate-limit, thread/turn, model-inference, full-workspace
or alternate-version evidence.

U14's focused acceptance gate covers the complete settings-browser slice rather
than rerunning the workspace after each edit. Warnings-denied all-target clippy
passes for UI/TUI/CLI. All **51 heycode-ui**, **136 heycode-tui** and **104 heycode-cli**
tests pass: schema completeness and valueless secrets, effect disposal, all four
editable control classes, stale-CAS recovery, frame/state priority, exact
service/command inventory, and a real-composition command → shared panel inbox →
browser-row path. The first CLI run found one exact service-baseline drift from
concurrent accepted work; the expected `settings-ui` and
`lmstudio/model-control` rows were added, then only that failed target was
rechecked. The last full workspace milestone remains the separately recorded
2,854-test gate; this focused result does not replace it.

PLM04's focused gate runs after the complete Settings→command→provider→readback→
catalog slice is assembled. Warnings-denied all-target clippy passes for LM
Studio and CLI; all **108 provider** and **105 CLI** tests pass. Deterministic
HTTP sequences prove that browsing/planning sends nothing, load uses the latest
Settings values in the exact native body, the provider echo and fresh native
instance list agree, and the shared catalog commits its next revision. Unload
requires an already observed instance, exact response id and confirmed removal.
Real composition proves the default plugin, namespace, queued command,
inventory and TUI settings rows; incomplete command syntax fails before local
HTTP. No installed LM Studio, external JIT mutation, inference turn,
full-workspace or cross-platform observation is claimed.

S13's focused gate treats data minimization as a type property, not a snapshot
redaction exercise. All **59 heycode-config** tests and warnings-denied all-target
clippy pass. Adversarial Codex TOML, Claude strict JSON/MCP JSON and OpenCode
JSONC fixtures include secret canaries in credentials, environment, headers,
helpers, hooks, credential-bearing URLs and unknown fields; none can enter
preview Debug or a candidate because excluded/unknown public rows have no value
slot. Project trust, executable authority, incomplete enabled MCP metadata,
collisions, source-byte immutability and detached no-I/O candidate construction
are covered. Current official format references were rechecked. Discovery,
confirmation, source CAS and persistence remain MCP14/later evidence.

E06/E08's focused gate joins storage, process, tool, durable provenance and UI
instead of accepting the lower crates alone. Warnings-denied clippy passes for
Core/Exec/Tools/Agent/TUI/CLI; accounted suites are **66, 81, 81, 159, 137 and
106** tests respectively. Real contained stdio fixtures prove initialize,
definition, references, document sync/diagnostics, per-request cancellation and
descendant teardown. Tool fixtures prove safe server discovery, exact normalized
JSON, >64 KiB spill through the complete-envelope cap and LSP-specific
untrusted classification. Real composition proves the three default plugins,
two services, four exact tool rows and an empty listing with no process. The
single CLI failure was expected service-map order (`lmstudio*` sorts before
`lsp`); the assertion changed and only that exact target was rerun. No trusted
project definition, real language server, IDE, non-Unix retained backend or
newer full-workspace claim is made.

A08/A09's focused gate joins Agent, Session and production composition.
Warnings-denied clippy passes; all **162 Agent**, **133 Session** and **107 CLI**
tests are accounted green. A 5,000-row deterministic catalog fixture measures
serialized schema bytes and JSON-node work rather than unstable wall time;
default lexical tests prove small-catalog identity, oversized relevance/stable
caps and pre-cancellation. Loop tests prove exact step/token/time/tool/unknown-
usage reasons, restart projection and Settings defaults. A real composed turn
proves both default effects, the restart-applied namespace, one pre-step layer
and full current small-catalog selection. The one failure was a stale assertion
expecting generic `error`; after the session vocabulary gained `max_steps`, only
that exact target was rerun. Provider-specific Code Mode opaque state and
full-workspace/platform evidence remain separate.

PLM05's deterministic product gate is green at **110 provider** and **110 CLI**
tests with warnings-denied clippy. Provider fixtures join five read-only Ollama
surfaces, withhold embeddings/non-completion rows, preserve independent
tools/vision/thinking evidence and prove the Chat wire separately. Real
composition with an explicit provider/model mounts the conditional plugin,
four services, shared provider/catalog descriptors and no-credential route
without network/process I/O; a credential reference fails before lookup. This
is picker/reachability evidence, not the required live chat. `command -v
ollama` is empty on this host, so no daemon/model was started or pulled and the
tracker row remains active.

MCP14's exact four-test gate passes over S13→MCP typed boundaries. Canaries in
headers/environment/OAuth and changed excluded values prove public previews and
deterministic unresolved references retain no credential bytes; clean stdio/
HTTP rows become exact private definitions, while trust/executable gaps,
enabled incomplete rows, invalid cwd/name and collisions return no partial set.
MCP crate formatting and warnings-denied clippy pass. Its broader package run
reached 8 unit + 289 integration tests; three pre-existing real-socket tests
could not bind under the separate thread sandbox and are explicitly not claimed
as passing. No live server, credential binding, discovery UI or persistence ran.

CAT06's focused warnings-denied gate passes **384 LLM**, **26 catalog-file**
and **36 OpenRouter** tests. Value constructors reject advisory facts without
source/capture, OpenRouter's production full/detail catalog join captures one
safe generation instant, and exact prices retain currency/unit/integer amount.
Schema-v2 persistence round-trips provenance; v1 fixtures prove source-less
facts become Unknown and current malformed provenance fails loud. CAT07's 18
override tests also pass for provenance forgery, field precedence,
contradictions, inert unknown models and cache non-contamination. TUI/CLI gates
now prove composed user/trusted-project service ownership and visible picker
assertion/conflict/unmatched labels without raw capability promotion. CAT07
now also proves machine enforcement cannot promote unevidenced/contradictory
support or larger/unknown limits; any future product Consumer of a narrowing
constraint must make its exact attribution durable through C02/C05 first.

Current evidence: the baseline test asserts the exact applied plugin order, service-owner map, command list, tool list and provider list. Real-binary tests cover generated-home migration and custom-home preservation using isolated temporary homes; the U10 composition test explicitly disables the system Keychain and uses an isolated file-provider root. No test reads or mutates the developer's real config.

### Tier D — replay and snapshots

Replay fixed provider/event fixtures through the real binary and compare:

- JSONL session.
- Canonical provider requests.
- Transcript frames.
- ACP/app-server events.
- Redacted diagnostics.

Fixtures run on all platforms. Normalizers may remove timestamps/ids only where the contract declares them nondeterministic.

### Tier E — live provider

Live tests are opt-in, rate/cost bounded and never required for ordinary contributor runs. They validate current provider behavior, auth, model catalog and native features.

Each artifact records provider, exact model, protocol, capability revision, region, latency, tokens, cache, cost and redacted outcome.

## Live provider matrix

| Provider/runtime | Required live checks |
|---|---|
| OpenRouter | catalog, `z-ai/glm-5.3-flash` text smoke, tool call, routing fallback, server web search |
| DeepSeek | current catalog/defaults, thinking/non-thinking, reasoning tool continuation |
| OpenAI API | Responses stream, hosted tool, token/cache usage, native compact |
| Anthropic API | Messages stream, thinking tool loop, server tool, token count, compact/context edit |
| MiniMax | model list, OpenAI and Anthropic protocols, interleaved tool state |
| Z.AI | general and eligible coding endpoint, thinking/tool loop, native/MCP web |
| LM Studio | endpoint/version, model list, load state, tool-capable local turn |
| Bedrock | auth/region, list models, selected protocol stream/tool/cache |
| Vertex Gemini | ADC/project/location, function call, thought signature, grounding |
| Vertex Claude | model access, Messages stream/tool state |
| Codex runtime | account/model list, ephemeral turn, permission request, compact |
| Claude runtime | auth/model/effort, no-persist turn, permission callback, compact/context status |
| OpenCode runtime | ACP/server connection, GLM-5.3-Flash turn, session resume |

Provider tests self-skip only when their declared credential/runtime prerequisite is absent. A configured but invalid credential fails the live suite; it is not treated as absent.

## GLM-5.3-Flash reference lane

`z-ai/glm-5.3-flash` replaces the anonymous `stealth/ox-alpha` development identity. Official Z.ai release material confirms they are the same model lineage; OpenRouter publishes the exact current slug and capabilities. The prior configured OpenCode run proves only the pre-release alias path. QLIVE01 now supplies a scheduled/manual production-composition lane: forced live catalog, reasoning evidence, text turn, exactly-once client-tool loop, durable header checks and a content-withheld seven-day freshness artifact. It requires a process-scoped `OPENROUTER_API_KEY` so the environment provider outranks this host's known dummy Keychain row. Missing credentials write `Skipped(NoCredential)` and fail; no authenticated heycode evidence is claimed until a fresh Passed artifact exists.

Use it for:

- Catalog and capability parsing.
- Stream normalization.
- Reasoning-level mapping.
- Tool-call round trips.
- Large-context pressure tests within provider limits.
- OpenCode comparison runs.

Do not make it the only quality model. Provider routes and catalog facts can
change or disappear. Every fixture records the exact catalog revision and date,
and a public catalog refresh never substitutes for the authenticated lane.

## Provider conformance lab

A shared harness drives protocol adapters with generated stream fragmentations and failure points.

Implemented Q02 foundation: `SseConformanceFixture` generates whole-body, bytewise and every two-fragment split from raw provider bytes. `SseFixtureCase` adds explicit terminal transport failure without treating it as clean EOF. `SseFixtureTransport` runs the production `SseDecoder`; `run_sse_conformance` gives each case a fresh service, stable label and transport-call count. Chat Completions and Responses both pass the same runner. Provider-specific semantic/error/retry dimensions extend this foundation in P08/CAT08/Q08 rather than forking it.

Test dimensions:

- Every split point across SSE lines, UTF-8 and JSON chunks.
- Missing/delayed/duplicate usage.
- Finish before usage and trailing frames.
- Multiple choices and multiple tool calls.
- Partial tool id/name/arguments.
- Reasoning and provider-state deltas.
- Mixed native server and client tools.
- Pause/continuation states.
- Mid-stream disconnect and resumable/non-resumable retry.
- 400/401/403/404/408/409/429/5xx classification.
- Retry-after variants.
- Context overflow.
- Model retirement/unknown model.
- Cancellation at every lifecycle stage.

Contract fixtures come from official examples or redacted captured responses and include source/version metadata.

## Agent quality evaluation

Compare heycode and reference clients on matched runtime/model/permission conditions.

### Eval categories

- Repository comprehension.
- Small bug fix.
- Multi-file feature.
- Test failure diagnosis.
- Refactor preserving behavior.
- Dependency/API migration.
- Frontend from screenshot/spec.
- Long tool-heavy task requiring compaction.
- Parallel research/subagent task.
- Recovery from tool/provider failure.
- Security-sensitive task under restricted permissions.

### Metrics

- Task success verified by deterministic tests.
- Correct files changed.
- Regression count.
- Tool-call validity.
- Tool retries and duplicate mutations.
- Wall-clock and TTFT.
- Input/output/reasoning/cache tokens.
- Cost.
- Human interventions and approvals.
- Context compactions and lost-context failures.
- Final answer accuracy and evidence.

Use enough repeated trials for stochastic comparisons and report confidence intervals. “Beats” requires a defined benchmark and statistically supported result, not anecdote.

## Performance program

### Benchmarks

- CLI argument parse and configuration load.
- Time to first TUI frame.
- Time to composer readiness.
- Plugin/profile composition.
- Session open/replay at 1K, 10K and 100K events.
- Markdown rendering and scrolling.
- Tool registry assembly at 15, 100, 1K and 10K tools.
- MCP startup and reconnect.
- Provider request resolution.
- Stream-to-frame latency.
- Parallel tool scheduler throughput.
- Compaction selection and token metering.
- Graceful shutdown with active children.

### Budgets

Budgets are defined in the master plan and enforced by benchmark regression thresholds. CI uses stable relative thresholds; release machines record absolute p50/p95/p99.

Profile startup must render before optional MCP/plugin health completes. Expensive catalog refreshes run in background and use cached data for initial UI.

## Security program

### Threat model

Actors and inputs:

- Malicious repository and instructions.
- Malicious dependency/build script.
- Prompt-injected web/MCP/tool result.
- Compromised MCP/plugin marketplace.
- Malicious provider response.
- Credential-stealing tool/process.
- Symlink/path race.
- Sandbox escape.
- Cross-session data leak.
- Delegated-runtime protocol spoofing.

Assets:

- Source code and uncommitted changes.
- Credentials and subscription sessions.
- Files outside writable roots.
- Cloud accounts and external systems.
- Session history and private reasoning.
- Plugin integrity and managed policy.

### Security tests

- Secret canaries across logs, session, prompts, child env, crash dumps and support bundles.
- Project config cannot activate before trust.
- Sandbox read/write/network matrix on macOS/Linux/Windows.
- Symlink swaps and path canonicalization.
- SSRF including DNS rebinding, redirects, IPv4/IPv6 and metadata endpoints.
- MCP annotations cannot bypass approval.
- OAuth state/PKCE/redirect/resource/issuer/client binding and token refresh.
- Plugin package/executable digest, grant, contribution-generation, signature and dependency substitution.
- Malformed provider/MCP JSON and output bombs.
- ANSI/control sequence sanitization.
- Cross-agent/session ownership.
- Cancellation and process-tree cleanup.

Security findings block release according to documented severity policy.

## Reliability and chaos

Inject failures at:

- Every plugin apply/dispose boundary.
- Credential store read/write/lock.
- Session append/flush/index update.
- Provider connect/stream/retry.
- Tool prepare/dispatch/commit.
- MCP initialize/list/call/reconnect.
- Subprocess spawn/kill/wait.
- Compaction start/summary/commit.
- Delegated runtime start/permission/close.

Properties:

- No committed event sequence gaps.
- No model-visible UI item without a durable source.
- No duplicate side effect after retry.
- First terminal settlement wins.
- Cleanup reaches quiescence or reports bounded orphan diagnostics.
- Last good settings/catalog/tool generation survives failed reload.

Implemented Q01 foundation: `heycode_cli::testing::RealCompositionHarness` creates isolated settings, credentials, sessions, catalog and workspace paths, then invokes the production `compose_world` factory/loader. It swaps only inference for `FakeProvider`. The consumed result owns both `Context` and `TempDir`; explicit/drop shutdown unwinds effects before filesystem cleanup. The exact default plugin/service/tool/command inventory audit runs through this harness, so every new default contribution joins the common proof.

## Cross-platform CI

### Per pull request

- macOS arm64.
- Linux x86_64.
- Windows x86_64.
- Formatting, clippy, tests, doc links, dependency policy.
- Keyless provider/MCP/replay fixtures.
- TUI snapshots for terminal capability profiles.

### Scheduled

- Live provider canaries.
- Linux Landlock and bubblewrap execution.
- Windows sandbox and process-tree tests.
- macOS Seatbelt denial tests.
- Plugin marketplace/update canary.
- Performance benchmark suite.
- Long-session stress and fuzz.

### Release candidate

- All supported platform installers.
- Fresh-machine first-run journey.
- Upgrade from each supported config/session version.
- Rollback to prior release.
- Provider live matrix.
- MCP OAuth with at least two real servers.
- Subscription runtime smokes.
- Security scan and dependency audit.

## Fuzzing and property tests

Targets:

- Session event parser/migration.
- Provider stream parser.
- Tool schema subset.
- Config patch/migration.
- MCP frames and pagination.
- ANSI/Markdown renderer.
- Path/glob logic.
- Compaction range selection.
- Plugin manifest and dependency graph.

Property examples:

- Serialize/parse round trip is lossless.
- Projection is deterministic.
- Compaction never splits required tool pairs.
- Native compaction never crosses provider/model/protocol and never shadows its settlement/future.
- Deny cannot become allow later in a policy chain.
- Disposal removes all owned registrations.
- Model-ordered results remain ordered under arbitrary completion timing.

## Observability and diagnostics

`heycode doctor` produces human and JSON forms with no secret values.

Checks:

- Binary/version/update.
- Config layers and migrations.
- Workspace trust.
- Plugin tree and activation failures.
- Credential references, source, writability and last validation.
- Provider endpoint and model catalog health.
- Retired/unknown configured model.
- Sandbox backend and kernel support.
- MCP transport/auth/tools/resources/prompts.
- Delegated runtime version/auth.
- Session/index integrity.
- Child process/orphan state.

S11 implements the shared schema-v1 registry and the settings, credential-registry and zero-apply K08 graph checks in a restricted diagnostic world. `doctor --composition` separately adds K08's body-free isolated activation phase: disposable product state, fake inference and explicit watcher/resume/MCP suppression prove plugin transactions without claiming live authority. The remaining rows above are still owned by their provider, trust, sandbox, MCP, runtime, session and telemetry tasks; neither diagnostic plane is evidence that those checks already exist.

Support bundles include redacted logs, config descriptors, version/platform, plugin inventory, health output and selected session trace only after explicit preview.

## Packaging and updates

Ship signed binaries/installers for macOS, Linux and Windows. Installation never requires a Rust toolchain.

Update channels:

- Stable.
- Preview.
- Pinned version.

Updates are atomic and preserve the previous binary for rollback. Configuration/session migrations create backups, are idempotent and have downgrade guidance.

Q14–Q16 now have a strict lower boundary, product Consumer and definition-only
workflow. `release-manager-gh` invokes GitHub's offline-bundle verifier through
the composed subprocess policy with cleared inheritance, isolated process
state and exact repository,
workflow, OIDC issuer and source tag. `heycode release apply|rollback` snapshots
enabled plugin API ranges at operation time, owns fresh/update policy and
re-verifies directional rollback. Transition journaling and config guidance
protect publication. The workflow refuses a ref other than
`refs/tags/v<version>`, uses OIDC attestations and defines deterministic
three-OS fake-turn smokes. No actual heycode attestation bundle has passed the
shipping command and no macOS/Linux/Windows real-provider matrix has been
observed, so Q14/Q15/Q16 remain active.

Q17 now supplies deterministic saved-version evidence: unversioned config and
every schema v1–v26 upgrade once, replan to none, preserve byte-exact backups
and produce compatible/restore/copy downgrade guidance; v27 refuses unchanged.
Session fixtures cover v1, v2, mixed v1-prefix/v2-suffix and future v3 without
rewriting historical lines. This is local format evidence, not a fresh-machine
or installer claim.

Plugin updates are independent from binary updates but constrained by API compatibility and managed policy.

## Release stages

### Developer preview

- Architecture may migrate aggressively.
- Provider support marked experimental/preview.
- No data-loss or secret-leak tolerance.

### Public beta

- Stable session v2 and plugin manifest v1.
- Tier 1 providers supported.
- First-run, doctor, MCP and plugin UX complete.
- Upgrade/rollback tested.

### 1.0

- Stable CLI/config/session/plugin contracts with documented deprecation policy.
- Certified provider/platform matrix.
- Performance and eval baselines published.
- Support/security processes operational.

## Release checklist

- [ ] Every advertised feature is reachable from the default or documented profile.
- [ ] `FEATURES.md` matches real-composition and live evidence.
- [ ] All deterministic gates pass.
- [ ] Required live canaries pass within freshness window.
- [ ] No critical/high unresolved security issue.
- [ ] Migrations and rollback pass on production-like data copies.
- [ ] Installers pass fresh-machine onboarding.
- [ ] Changelog names breaking/config/provider changes.
- [ ] Documentation links and examples are current.
- [ ] Provider model defaults are live-discovered or validated non-retired ids.
- [ ] Context shutdown and child cleanup are verified end to end.
- [ ] Support bundle is redacted and previewed.

## 2026-08-31 local gate and external-evidence boundary

The integrated 59-crate/schema-28/115-plugin tree passes workspace formatting,
all-target warnings-denied clippy and 3,749 unit/integration tests plus 7
doctests (0 failed, 0 ignored). The integration pass caught stale/invalid
schema fixtures and the derived `open_ai_responses` spelling; fixtures now track
schema 28/29 correctly and `openai_responses` is canonical with a read alias for
the accidental spelling. Focused failed-target reruns and the complete
post-documentation workspace rerun are green.

Platform evidence now includes native macOS Seatbelt and real Linux/arm64
Docker bwrap/path/TOCTOU execution. LinuxKit exposes no Landlock ABI v1 and no
native Windows runner is available, so E11/E13/Q07/QSEC03 remain active. Every
dedicated workflow invokes the actual consolidated Cargo test binary; a workflow
definition or cross-target lint is still not a native pass.

The local release transaction uses a real built heycode artifact for fresh install,
channel/plugin-API refusal, update, directional rollback and fake turns. The
verified candidate's own command performs installation and onboarding executes
the exact published stable path. Its signature verifier is explicitly a fixture;
Q14–Q16 still require genuine hosted attestations and three-platform real turns.

The five fuzz targets remain isolated from the shipping workspace. Local fixed-
seed ASan smoke, including the ANSI/OSC render regression, is green. Q12 remains
active until the scheduled hosted continuous job is observed.
