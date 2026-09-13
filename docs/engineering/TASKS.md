# Ship-ready implementation tracker

This tracker is the execution source for the engineering program. Every code task is TDD: add the failing contract test, observe the intended failure, implement, run focused verification, then update the affected architecture/status documents.

Status: `[ ]` not started · `[~]` active · `[x]` complete · `[!]` blocked.

Current rollup (2026-08-31 MCP/provider-policy integration): **254 complete · 31 active · 7 not started · 0 blocked**.

**A session restart ended 21 in-flight lanes at once** (GOTCHAS #186). Their file edits survived across 24 crates; their reports did not, so **no row they touched may be marked complete on their say-so — there is no say-so.** Six of the original recovered rows remain `[~]`: work exists on disk, and until the owner verifies each against its acceptance it is a candidate, not a delivery. C08, A06, MCP12, PMM03, PZA03, POA02, PAN02, Q06, TEL03, TEL05 and X02 have now passed direct owner acceptance and focused gates; K07/K08 separately closed their audited gaps. CMD04 was rejected back to `[ ]` because it implements only one of the five required panel-opening commands. Evidence for the remaining candidates is encouraging but not dispositive: no `todo!`/`unimplemented!`/TODO marker appears in their touched files.

**THE CURRENT FULL WORKSPACE GATE IS GREEN: 3,749 unit/integration tests passed, 0 failed, 0 ignored, plus 7 doctests.** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast` pass on the schema-v28/115-plugin Unix tree after MCP11/MCP13/PAWS05 product integration.

**The workspace compiles end to end again.** The first cold report was wrong and the reason is worth recording: the owner filtered it with `tail -60`, which *truncated* the error list rather than filtering it, hiding two broken crates behind `heycode-http`. Filter, never truncate. The complete list was five errors across three crates, all incomplete work from lanes that died mid-edit:
- `heycode-http` — `unwrap_err()` on a type deliberately lacking `Debug`, plus an unused `mut`. Fixed on the **test** side: `WebSocketRequest` has no `Debug` because its headers may carry authorization values, so deriving one to satisfy a test would have converted a security decision into a leak path (GOTCHAS #188).
- `heycode-agent` — three A06 test helpers typed `index: u32` where `StreamChunk::ToolCallDelta.index` is `u16`.
- `heycode-app-server` — X02 added `AppServerErrorCode::Unsupported` and the `runtimes`/`workspace` capability flags to `heycode-sdk` and died before wiring the server. `rpc_code` now maps `Unsupported` to `-32003`, matching what `heycode_sdk::client` decodes.

**X02 now closes the server half that its wire contract was missing.** Base app-server worlds still advertise `runtimes` and `workspace` as `false`; installing `app-server-controls` makes both true only after `runtimes/list`, `runtime/select` and `workspace/select` are dispatchable. Runtime rows retain native/delegated kind plus exact tri-state capability evidence. Provider/model/runtime changes refuse active turns; runtime backend publication follows the Routing Settings commit; opened or differently linked sessions reject a switch. Workspace selection is canonical, absolute, existing, directory-only and contained by the composed root. Native runtime remains fixed to its composed workspace; delegated runtimes receive the selected path at start/resume. The real production loader and typed SDK exercise capabilities, listing, current-runtime selection and the native workspace no-op.

**Five real defects were found in the dead lanes' failing tests, four of them security-relevant** (GOTCHAS #191-#194). The tests were written as requirements; the lanes died before resolving them, so they arrived as failures rather than as reports:

- **A read-only subtree was unenforceable.** `LocalFileSystemBackend::target` filtered roots by access *before* choosing the most specific one, so a write to a `ReadOnly` `workspace/protected` skipped that root and **matched the read-write parent instead** — the write succeeded. Selection and permission are now separate steps in that order: most specific root wins the match, then that root is obeyed.
- **A `Component::CurDir` check was dead code.** `Path::components()` normalises `.` away, so the traversal guard only ever caught `..`. A hand-built `ResolvedPath` containing `./` passed straight through. The check now splits raw segments.
- **Root substitution reported `NotFound`.** Removing a workspace root and renaming a decoy into place was correctly refused, but only as a bare ENOENT from the retained `Dir` handle — the substitution, the actionable half, was what the generic error hid. Identity is now verified before the first operation *and* at commit: two checks answering two different questions.
- **Two Seatbelt escape-matrix assertions forbade the payload the test itself planted**, making them unsatisfiable by correct output. Both restated to say *where* the payload may appear rather than *whether*. Test defects, not code defects — the sandbox escaped and carried the payload correctly throughout.

**A cancellation test in A06 was racy, not wrong.** `let _keep = releases;` sat inside the cancelling block, so the release senders dropped the moment it ended — making both `select!` arms ready at once and letting tokio pick at random. Held to after the driver, cancellation is the only way out of the select, which is what "nothing releases them" always meant.

**Repository initialized and committed** (`e40ff59`, local only, no remote, nothing pushed). 848 files, no build output, no credentials — scanned before staging. The absence of any commit had already cost recovery work twice (GOTCHAS #186, #189).

**A real SSRF bypass was found, and it was found by reading, not by a report** (GOTCHAS #187). QSEC03 left a *tripwire* test recording that `ip_is_private` used `to_ipv4_mapped` — which recognises only `::ffff:a.b.c.d` — so `::7f00:1`, `64:ff9b::7f00:1` and `2002:7f00:1::` all reached **loopback**, and eight special-purpose IPv4 ranges were reachable outright. The predicate now resolves the embedded IPv4 destination and recurses, so every IPv4 rule applies through every encoding; the tripwire is retired and its requirement test is a gate (`0 ignored`). Over-blocking is pinned in the opposite direction by `ranges_that_look_private_but_are_not_stay_reachable`.

**QSEC03 findings W2 and W3 are now fixed too.** A redirect may no longer downgrade https to http (upgrades stay legal); off-protocol ports are refused before connect — SSH, SMTP, MySQL, Redis, memcached, Elasticsearch, the Docker daemon, MongoDB — under a stated rule rather than a bare list (below 1024 only 80 and 443; above it only named datastore/admin ports), with 3000/5000/8080/8443/9000 pinned as still reachable; and the internal-name arm now covers the fully-qualified `localhost.`, the whole RFC 6761 `localhost` TLD, `.localdomain`, `.lan` and RFC 8375 `home.arpa`, so a lying resolver cannot admit them. `heycode-web` 24+19 tests, **0 ignored**.

**The remaining QSEC03 host-policy bypass is fixed, but the row stays active.** With any block rule, literal IPv4/IPv6 URLs are refused because they may be the translated form of a blocked name, and an IDN requires a matching IDN allow rule; a broad ASCII parent rule cannot authorize an unlisted punycode homograph. Ordinary unrelated ASCII names remain reachable, and default no-block policy retains public literals/IDNs. The same rule gates registry search output, fetch admission/final URL and every portable redirect hop. Seatbelt roots now encode quotes/backslashes as string data and reject control/non-UTF-8 roots; crafted-root native enforcement passes on macOS. Focused evidence is web **45**, exec **71**, sandbox **49**, fmt and warnings-denied clippy green. QSEC03 remains `[~]`: Linux and Windows runtime cells have not run natively, and Seatbelt truthfully retains a pre-existing-hard-link inode alias limitation.

**The owner deleted ten of that lane's security tests with an unanchored slice edit and recovered them from the subagent transcript** (GOTCHAS #189). The suite stayed green throughout, because the tests that would have failed were the ones deleted; only a test-count drop from 24 to 14 exposed it. Recovery needed the lane's Bash-heredoc edits replayed in order, not just its single `Write` restored.

**C08 and A06 are now owner-accepted, and A07 is implemented.** C08's `project_repair` is a read-time projection leaving JSONL untouched; torn final bytes are classified upstream as retryable `UnterminatedTail` and never become a repair record. A06 exhaustively checks 4,282 completion-order/barrier combinations plus 400 fixed-seed failure/denial cases and commits the model's order. A07 routes preparation, stream, invariant and tool-batch failures through one stage-aware owner: a started step closes before `turn/end error`, and failure UI publishes only after durable closure. Focused gates: heycode-session 132 and heycode-agent 135, both with package format and warnings-denied clippy green.

**Four rows were demoted from `[x]` to `[~]` by an audit of the owner's own completed work** (GOTCHAS #184, #185), and all four confirmed gaps are now closed. CAT06 was last: pricing/performance provenance now lives on the value itself rather than borrowing the enclosing generation.

**K08 is restored only after adding the activation phase its acceptance names.** `inspect_world` remains a strictly zero-apply graph API. The CLI command now joins that graph to a second production-loader activation in a disposable canonical workspace with temporary sessions/settings/credentials/attachments/catalog state, fake inference, no watchers/resume and no configured MCP transport. Human/schema-v1 JSON name every plugin's scope and `activated|failed|not_attempted` state plus the failure stage; raw activation text is structurally absent and deliberate suppressions are explicit. A successful probe shuts down the Context before deleting its root. Core **65** and CLI **102** tests plus warnings-denied focused clippy are green (GOTCHAS #207).

**K07 is restored only after adding the missing production consumer.** Plugin `profiles` publishes an effect-owned `NamedProfileService` over the exact `NamedProfileStore` used by CLI startup. TUI contributes queued `/profile [name]`, lists that service, highlights the current selection, validates direct or modal choices, restores/quiesces the shell, and returns a typed profile-recomposition outcome. The CLI replaces only the prior `--profile` pair, preserves every other original argument, shuts down the old Context/runtime, and reloads the selected layer through the authoritative startup path. Schema v21 inserts `profiles` and `commands` before historical exact TUI Consumers. Real-composition tests traverse service → command → typed event and the existing real-binary test proves the resulting layer changes the composed world.

**C11 is restored only after production reachability was added.** Strict adapter calls measure the actual `ResolvedCall`; compatibility calls measure the actual `ChatRequest`; Agent and native subagents inject the effect-owned counter registry and retain the last complete five-contributor envelope. Provider state is explicitly estimated from its lossless data, media is unmeasurable rather than zero, and exact-counter refusals remain visible. Schema v20 inserts `token-counters` before the first Agent Consumer in historical exact profiles.

**TEL05 is accepted only after wiring its recovered store.** The library already bounded entries, bytes, labels and checks; distinguished torn tails from rotted rows; protected recent failures without weakening the hard caps; screened both construction and deserialization; and reopened the same file after restart. It had no production factory or service. The default world now mounts `health-history` and `/health` at an isolated home-relative path, with exact inventory/service/command audits green. CMD04 remains not started: `/mcp` opens its panel, `/plugins` retains its inventory behavior, and `/skills`, `/agents`, `/hooks` have no panel variants.

PGCP04 was suspected and **refuted** — the fixture half is met, the live half was not executed, and three places say so, one naming the row id. A disclosed gap is not an overclaim.

`[~]` now covers two different situations and the distinction matters when reading this table:
- **10 awaiting external evidence or approval** — POR04, POR05, QLIVE01, PLM05, E11, E13, QSEC01, Q07, Q14 and Q15. Q14/Q15 now have their production verifier/manager/CLI; only an actual heycode attestation transaction can close them.
- **4 recovered implementation candidates** — PAWS04, PDS04, QSEC03 and PGCP05. A row here contains work, not accepted completion; it returns to `[ ]` if owner review rejects it. P09 has passed owner acceptance; CAT07 has passed owner review and stays active for its named product-visibility bridge.

**POA02 and PAN02 are accepted at provider-owned state scope, not product activation.** OpenAI's strict provider wrapper rejects neutral assistant history, unknown phase values and empty encrypted reasoning, while a three-turn stateless loop proves exact parser-published item/order/call-id/phase replay under `store:false`. Anthropic preserves complete signed and redacted thinking blocks at every tool step, distinguishes adaptive from manual ordering, and rejects the known adaptive-only default for manual construction. Current official docs confirm both protocols. OpenAI **31** and Anthropic **38** tests are accounted green with focused warnings-denied clippy; no CLI inference registration, credentialed live call or complete per-model thinking-dialect inventory is claimed (GOTCHAS #208/#209).

**POR06 is complete only after its provider policy crossed the shared wire and production factory.** The Chat adapter now accepts multiple unique option dialects and can project one exact object member without silently dropping siblings. Every `OpenRouterProvider` constructor requires a provider-owned `transforms` option; production supplies `OpenRouterTransformPolicy::all_disabled`, so routing and the full explicit `plugins` disable array both persist in `request/header` and reach top-level wire fields. Response healing remains impossible on the streaming/non-structured route, and effective execution remains Unknown under account “Prevent overrides.” LLM **383**, OpenRouter **36** and CLI **102** tests are accounted green with warnings-denied clippy (GOTCHAS #210). POR04/POR05 remain active for their independent trustworthy-live evidence.

**PMM05 is complete; PMM04 is active at the remaining product bridge.** Current MiniMax primary sources replace the former Coding Plan with Token Plan and say a Token Plan key may exist before a seat or Credits makes it usable. `MiniMaxCodingProfile` therefore requires affirmative seat/Credits evidence, retains the Token Plan identity/credential and documented `/v1|/anthropic` routes, and leaves a dedicated historical endpoint Unknown. The current MCP guide also documents both `web_search` and `understand_image`; root corrected the recovered stale legacy-only classification. `AllDocumented` installs both with prompt/untrusted policy, while `WebSearchOnly` is explicit least privilege. MiniMax **125** tests plus four doctest cases are green. PMM04 remains `[~]` until a composed MCP owner converts the secret-free bundle, resolves its credential at launch and proves connection/tool discovery (GOTCHAS #211).

**PZA04 is complete; PZA05 is active at the same credential-aware MCP bridge.** Core's durable `ServerToolSource` now has optional bounded web metadata for site/icon/provider-reference/publication while legacy rows deserialize with it absent. Z.AI projects all seven documented search fields into `server-tool/result`; a real session append/reopen proves none are lost. An opt-in `native-zai` production factory registers exact N01 candidate `zai:web_search`, with provider-specific selection and inventory/disposal tests. Core **65**, session **133**, Z.AI **68** plus two doctests, and CLI **103** tests are green. The four-server Coding Plan bundle remains `[~]`: its specs/rollback are complete, but Streamable HTTP/stdio connection owners still refuse unresolved credential references, so no product tool generation exists (GOTCHAS #212).

**U14 and PLM04 are complete; PLM05 is active only for live evidence.** The default world exposes effect-owned `settings-ui`, and `/settings` opens every schema-derived/custom namespace over authoritative snapshots. Provider plugin `lmstudio-control` owns live `lmstudio-load` plus queued `/lmstudio <load|unload> <target>` with exact readback/catalog refresh. Ollama remains a distinct product. When configuration explicitly selects `provider="ollama"` and a model, conditional `provider-ollama` publishes joined catalog/profile/inference/inspector services, the `llm` bridge registers its no-credential Chat provider, and normal provider/model picker registries see identical evidence. Composition performs no Ollama request or process action; any credential reference fails before lookup. The joined catalog later uses version/tags/ps/show/OpenAI models read-only evidence. Provider **111** and CLI **130** focused inventories include a five-surface no-tools/content-withheld live gate. No `ollama` executable or daemon is available on this host, so the row stays `[~]` solely for its required installed-model chat smoke (GOTCHAS #222/#280).

**S13 is complete at its preview boundary and unlocks MCP14.** `heycode-config`
strictly parses already-read Codex TOML, Claude settings/MCP JSON and OpenCode
JSON/JSONC under host-supplied user/project/executable authority. Public preview
types retain only screened provider/model/base URL, credential-free MCP
transport and typed settings metadata. Credential/environment/header/OAuth/
helper/hook fields have no value slot; unknown fields expose path and structural
kind only. Candidate construction clones a typed Config, clears route credential
pointers, refuses collisions and incomplete enabled MCP rows, and performs no
I/O. Source discovery, UI confirmation, source CAS, credential-reference
creation and persistence are deliberately MCP14/later work, not hidden claims.
Official current formats were rechecked and all **59 config** tests plus
warnings-denied clippy are green (GOTCHAS #219).

**MCP14 is complete at its unresolved-reference preview boundary.** It accepts
only S13's screened public preview—never source bytes/paths or credential/
header/environment maps. Clean rows become exact private definitions with
reconnect disabled. Credential-shaped exclusions become deterministic
`McpSecretReference` requests carrying only source path and binding role; no
header/environment name or value is invented. Project/executable authority,
enabled incomplete rows and existing-name conflicts fail the whole candidate
set, while disabled incomplete rows stay visible and unapplied. Four exact
MCP14 tests pass; crate fmt/clippy are green. Three unrelated pre-existing
real-socket tests could not bind inside the separate thread sandbox and are not
reported as passing or as MCP14 failures (GOTCHAS #223).

**CAT06 is complete; CAT07 remains product-active.** `ModelPricing` and
`ModelPerformance` now require bounded `ModelMetadataProvenance { source,
captured_at_ms }` whenever facts are non-empty. OpenRouter captures one safe
source/instant per successful generation; absent pricing remains provenance-free
Unknown. Cache schema v2 persists value-level provenance; schema v1 remains
readable but drops source-less advisory data to Unknown rather than borrowing a
generation timestamp. Focused gates pass at **384 LLM, 26 catalog-file and 36
OpenRouter** tests. CAT07's layered override service also passes 18 adversarial
tests: assertions cannot spell provenance, mutate/cache provider evidence, add
models or hide contradictions. Default composition now loads user plus trusted-
project layers, and TUI visibly labels assertions/conflicts/unmatched models
without letting them satisfy raw provider-capability filters. It stays `[~]`
because asserted limits/capabilities do not yet enter the durable request/C05
verification plane; visible preview is not functional enforcement (GOTCHAS #224).

**K11 and PL08 are complete.** Standalone profile schema v2
keeps v1 plugin rows readable and adds managed-only implementation-source and
denied-capability rules. User/project/local/session layers cannot claim that
authority. The production factory constructs every selected descriptor,
enforces the final managed rules, and only then returns activatable plugins; a
forbidden source or external-process family registers zero effects. Config
**65** tests, exact production rejection and warnings-denied core/config/CLI
clippy are green (GOTCHAS #232).

**PL08 closes both managed admission and the default-product bypass.** The
extension boundary makes all eight source/channel/publisher/version/digest/
signature/platform/capability decisions explicit, freezes bytes before policy,
mutates no cache state on refusal, and rehashes/rechecks current policy before
declarative host callbacks. Verified signature and upstream checksum evidence
remain Unknown/denied. Production lifecycle requires an admission provider:
missing authority denies install/enable/update/rollback, while disable/remove
remain available; `ManagedLifecycleAdmission` is the exact allowed-generation
adapter. Extension **107** tests and the production CLI denial/clippy checks are
green (GOTCHAS #235).

**PL07 and P10 are complete.** PL07 resolves a complete manifest generation for
one explicit platform into deterministic dependency-first order; distinct
duplicate/missing/version/conflict/cycle/platform failures precede frozen cache,
single-write lifecycle or ordinary/managed activation mutation. Extension
format/clippy and **119 tests** are green. P10 publishes effect-owned strict
request/response seams from every `llm` implementation. Auth preview/final
binding, downstream native-tool routes and durable C05 reconstruction are
independently checked; response layers receive normalized events or body-free
failure classes before optional telemetry/session/UI. Missing `next`, layer
errors, refusal and cancellation fail/settle explicitly. Current Consumers are
Agent auth/native-tool admission and optional default `provider-telemetry`;
local-off and opt-in OTLP composition both pass. Scoped factory build stably
orders late providers before Consumers without changing independent profile
precedence (GOTCHAS #238/#239). That boundary unlocked N05.

**N05 is complete.** Optional default `request-transforms` publishes an
effect-owned provider registry plus one post-`next` P10 layer; provider plugin
`request-transforms-openrouter` contributes context-compression, file-parser
and response-healing rows. Descriptor generations freeze at registration,
dispose exactly and keep requested/effective/effect/cost separate. Disabled
means no cost; enabled requires explicit Unknown, documented-free,
upstream-token or validated nonzero page pricing. The layer inserts an absent
exact option, accepts equality and refuses conflict before adapter/C05
transport admission. LLM **398**, OpenRouter **38**, core **69** and CLI
**118** tests plus warnings-denied clippy are green (GOTCHAS #240).

**N06 is complete without inventing provider calls.** Core validates positive
aggregate counts plus Unknown/published cost evidence. Chat normalizes
OpenRouter's documented `web_search_requests` aggregate; Agent commits v2
`server-tool/usage` only with the successful output group. Request projection
correlates one logical aggregate per request. TEL01 `/usage` keeps local,
provider-exact and provider-aggregate rows separate, derives exact outcomes and
unsettled counts only from ids, dedupes invalid repeats and never renders
Unknown as free. Core **70**, session **148**, status **28**, LLM **398**,
Agent **172** and OpenRouter **38** pass with warnings-denied clippy
(GOTCHAS #241). TEL04 now consumes those committed facts.

**TEL04 is complete at the committed-session boundary.** Optional default
`telemetry-metrics` listens to the session-owned committed event bus, seeds
route/lineage/runtime correlation without replaying historical metrics and
emits only closed provider/tool/compaction/cache dimensions. Provider
aggregates preserve their positive count instead of becoming one invocation;
schema-v1 telemetry reads as count one and schema v2 rejects zero. Local-off
still has no exporter, while the opt-in OTLP provider receives the same
content-free event contract. Agent **173**, telemetry **119** and OTLP **15**
tests are focused green (GOTCHAS #242).

**Q03 and U19 are complete as one accessible-shell vertical.** The reusable
journey harness drives the production `AppState` reducer with explicit host,
terminal and UI stimuli and captures exact flat frames for trust, first-run
setup, command discovery, MCP management and provider/runtime selection. The
same screen-reader projection is bounded, control-sanitized and duplicate-
suppressed in production; flat mode writes no alternate-screen/cursor-control
bytes and automatic `TERM=dumb` uses it. Shipping `--screen-reader` now selects
that mode only for interactive TUI startup and survives connection/profile/
session/trust recomposition with every other original argument. TUI **158** and
CLI **119** tests are focused green (GOTCHAS #243).

**P09 is complete only after replacing the production frozen-key bridge.** The
shared protocol adapters already acquired one `RouteCredential` per operation,
but root composition resolved a secret and fed fixed-key constructors. The
shipping DeepSeek, OpenRouter, OpenAI and Anthropic routes now retain the exact
configured registry handle after preflight; retries share only their operation
value, the next request observes rotation and a resolver mismatch fails before
registry access. Shared wire fixtures prove rotation/no-fallback across five
protocol adapters, while four production compositions prove exact custom auth
bindings without network traffic (GOTCHAS #244).

**A05/U18 are complete for the native durable-inbox plane.** During native
active work, Enter appends a Steer for the next model step, Tab appends a
FollowUp for a later turn and Esc only interrupts. Text is not narrated or
model-visible until Agent's atomic claim publishes the durable user message.
Queue counts and state-specific keys render in full and flat modes; one Wake
starts one cancellation-owned follow-up, remaining messages wake again after
settlement, and resume seeds the same state. A visual-turn settlement race
converts an otherwise stranded idle steer into a cancel-recorded follow-up.
Delegated runtimes stay visibly unavailable rather than sending to the native
Agent. UI **51**, TUI **164** and the eight Agent inbox tests are green
(GOTCHAS #245).

**O02/O03 are complete at the native subagent boundary.** Fresh one-shot
children see only their prompt, forked children use verified shared-prefix
lineage without copied history, and continuable children reuse one durable
session. Registry-minted authority binds an opaque owner, depth and retention
right; nested list/send/interrupt cannot observe foreign siblings, unbound
requests fail before provider execution, one-shot children cannot leave live
descendants and follow-ups retain depth. Provider handle presence/identity is
checked before publication and Context disposal interrupts every remaining
owned child. Agent **178** and downstream CLI **119** tests are green
(GOTCHAS #246). O07 is newly optional/actionable after O02.

**QSEC02 is complete as one premise-checked product canary.** A deterministic
environment credential is positively observed at resolution and the strict
adapter Authorization header, then the same value is proven absent from the
provider body/system prompt, UI/debug, request projection, physical JSONL,
process diagnostic and committed structural support export. A separate red
regression exposed free-form CredentialProvider errors copying arbitrary text;
the registry now discards that body and retains only safe provider/reference
ids. Credentials **12**, CLI **120**, and the supporting Exec **81**, Session
**148**, LLM **398**, Agent **178** matrices are focused green
(GOTCHAS #247).

**Q04 is complete with one physical replay oracle.** The shared Agent testing
boundary commits a fixture's exact C02 snapshots, flushes and reopens JSONL,
finds the exact request id, projects and invokes production C05 against the
same adapter. Chat, Responses, Anthropic Messages, Gemini and Bedrock use one
matrix. A persisted-byte corruption fails while the writer's in-memory events
remain valid, proving the oracle reads disk; errors are body-free. Agent
**180** tests and warnings-denied clippy are green (GOTCHAS #248).

**U15 and CMD06 are complete as one lifecycle vertical.** The TUI owns a
ten-row deterministic session browser with bounded text/storage/lineage/status/
source/cwd/runtime filters, current/latest/archive markers and visible corrupt
store failures. The lower JSONL service owns create/resume/fork/rename/archive/
delete/export; archive never moves a fork path, delete is cancel-default and
refuses current/open/ancestor targets before a recoverable trash move, and a
lossless export bundles byte-exact validated ancestor suffixes. One owner-only
cross-process lineage lock serializes fork/delete/restore/export so a child
cannot appear between safety proof and rename. CLI recomposes from the exact
committed id; schema v23 repairs historical exact TUI profiles. Redacted support
bundles remain C10 rather than being implied by lossless/Markdown export
(GOTCHAS #233).

**C10 completes the third export form without copying session content.**
`/export support` writes a bounded schema-v1 structural JSON trace: static kind
names, seq/time, closed outcomes, counts/booleans and numeric usage/cache/edit
facts only. Prompt/answer/reasoning/tool/provider/URL/title/path/id fields do
not exist in the output schema. A canary-bearing real session proves none of
those values serialize, while the earlier lineage JSONL and Markdown tests keep
the lossless and human forms exact. Session **147** and TUI **149** tests plus
warnings-denied clippy are green (GOTCHAS #236).

**C12 is complete only after native state and production reachability joined.**
Default effect plugin `compactions` publishes `provider-native`,
`portable-summary` and `prune-oldest`; Agent and subagent Consumers inject it.
Strategies prepare read-only plans and the registry alone appends one settlement
after proving no strategy/concurrent durable mutation. `/compact`, pressure
middleware and native RuntimeSession all use the same portable row. Optional
provider-native transport returns a bounded exact-route checkpoint that commits
as v2-only `compaction/native`; same-route projection replays it, while neutral
or incompatible routes retain original history. V1 and self/future-shadowing
markers fail loud. Schema v22 repairs historical exact profiles. C14's
portable/fork/cancel policy and C15's 1,000-turn durable stress are complete
(GOTCHAS #145/#225–#227).

**POA04/05 and PAN04/05/06 are production-reachable.**
Credential-backed root composition now selects `OpenAiProvider` and
`AnthropicProvider`, whose strict adapters expose C12 native compaction. Isolated
owner-only credentials prove composition without network I/O. OpenAI posts the
buffered compact endpoint and replays exact user+opaque items; Anthropic merges
the beta edit, normalizes the compaction stop and retains complete assistant
state. Provider-owned restart Settings default both products off. OpenAI maps a
credential-screened key plus implicit/explicit mode into the exact Responses
option. Anthropic maps 5m/1h cache and every current thinking/tool-clear mode,
and the token counter consumes the same generation. Root resolves policy before
provider publication; schema v24 repairs exact provider/counter profiles.
Neutral detailed facts commit through `assistant/response-metadata` and usage UI
replays them (GOTCHAS #228/#231/#234/#237).

**DOC03 is generated and publicly linked.** Capability rows use exhaustive
`ModelCapabilities` destructuring, provider route classes use the binary's exact
selection constants and the checked-in Markdown has a regeneration drift test.
README, engineering evidence and STATUS link the single generated reference;
per-model data stays live-catalog-only (GOTCHAS #229).

**U16/CMD07 are complete through the durable detailed-response plane.**
Default `status-context` owns `/context` and `/usage`; the first renders every
latest envelope contributor/refusal, exact/estimated/uncounted total, durable
window bound, published-price cost or Unknown, latest exact cache/edit facts and
live compaction rows. The second renders durable session usage/routes/lower
bounds/cost completeness plus at most 20 detailed response and turn rows.
Cache-aware pricing requires an explicit non-overlapping partition and every
used price; ambiguity remains Unknown. `/compact` lists or runs any composed
strategy (GOTCHAS #230/#234).

**E06 and E08 are complete product verticals.** Unix default plugin
`retained-output-local` owns a unique owner-only generation under the product
root; SHA-256 objects, logical owner isolation, entry/object/generation caps,
verified bounded reads and effect cleanup are joined to a preview whose entire
wrapper/id/byte metadata/body/footer fits the caller cap. Default `lsp-registry`
uses the composed filesystem/subprocess/sandbox path; exact `lsp-stdio`
definitions are effect-owned, lazy and cancellation-safe. Default `lsp-tools`
contributes server listing, definition, references and diagnostics; results over
64 KiB spill and return E06's bounded preview unchanged. A zero-definition world lists empty and
starts no process. Language-server results carry their own durable untrusted
source, runtime notice and TUI warning. Focused gates account **66 Core, 81
Exec, 81 Tools, 159 Agent, 137 TUI and 106 CLI** tests (GOTCHAS #220). E08 now
unlocks optional X07 IDE proof; trusted language-server definition discovery is
still a separate configuration owner, not a reason to keep E08 open.

**A08 and A09 are complete defaults.** `deferred-tools` installs one bounded
`lexical-local` selection provider after Agent: current small catalogs pass
through byte-for-byte; catalogs over the explicit 64-row ceiling rank query
overlap with stable registry-order ties, and the same membership filters client
schemas, N01 routes and prompt tool names. Code Mode can materialize ordinary
legacy/strict tool-call events but owns no execution handle, leaving A06 as the
only approval/execution/commit owner. `loop-budget-settings` owns restart-applied
namespace `loop-budget` with explicit step/token/elapsed/tool limits and an
explicit missing-usage policy. Its pre-step layer reconstructs every counter
from JSONL. Session-owned turn-end reasons now distinguish max steps, elapsed,
tool calls, unreported usage and clock failure; native runtime maps all budget
stops to a limit. Focused gates account **162 Agent, 133 Session and 107 CLI**
tests (GOTCHAS #221).

**PAWS05 is complete; PAWS04/PAWS06 and PGCP05–07 retain their named hosted
evidence.** LLM, AWS-provider and Google-provider suites
tests cover selected-model/N01 option materialization; Bedrock cache points,
guardrails, cross-region route evidence and detailed cache usage; Gemini/Vertex
Search, external grounding, code and cache option/event projection; and an
exact-model Claude Vertex Messages wrapper. Agent now invokes the contextual
option hook before P10/header commit; Bedrock text/tool/opaque reasoning is
lossless same-route state; the maintained Google Developer default is a
provider-owned production route. Root now selects exactly one Mantle Responses
or Messages dialect and rejects incompatible protocol/model/output-cap tuples
before effects, satisfying PAWS05. Bedrock cache/guardrail evidence, Google
native/Vertex/Claude hosted behavior and every authorized live gate remain
independent (GOTCHAS #214/#264/#266/#278/#283).

**PDS05 is complete at its literal provider-capability acceptance; PDS04 remains active.** Strict tools and chat prefix use beta Chat but keep schemas/prefix text in their existing durable planes; JSON Output uses standard Chat with the documented prompt admission; FIM is a distinct beta `/completions` non-thinking request with a 4K cap. Contradictory current Flash evidence stays Unknown. Three compile-valid mutations were killed and all DeepSeek **70** tests pass. None of those facts supplies PDS04's missing authenticated Anthropic-format smoke (GOTCHAS #215).

**POA03/PAN03 are active provider-native tool substrates.** OpenAI **36** tests cover seven exact hosted-tool definitions, capability gates, completed-item classification and credential-free MCP configuration. Anthropic **43** tests cover six exact server-tool definitions, beta/request extensions, tri-state model gates, lossless call/result classification and pending pause state. Shared Responses/Messages normalization, durable event projection and production factories remain absent, so neither row meets its end-to-end normalization acceptance yet (GOTCHAS #216).

**Q06, PL03 and PL04 are accepted at their distinct lifecycle boundaries.** `GenerationContext` is the terminal owner of one plugin world and calls `Context::shutdown()` when the last strong reader releases it, including after the registry itself is gone. The generated lifecycle lab models clean/rejected reloads, retained readers, sweeps, exact seam withdrawal and terminal registry drop across 192 deterministic sequences. The concrete `product-extensions` Consumer now resolves enabled immutable-cache packages during apply and maps strict documents into token-owned skill, command, agent-preset, hook, theme and provider registries. Bundled stdio MCP reuses the ordinary connection owner after canonical package-root and permission checks, so transport/tool teardown keeps the same lifecycle law.

**MCP12 is accepted only after crossing the shared Tool/Agent/session boundary.** The MCP parser retains all five content kinds, annotations/extensions, explicit JSON null and honest subset-schema evidence; unknown kinds/assertions never become truncated content or false conformance. Registered tools now return a bounded typed pending result. Agent's ordered commit cursor admits image/audio/blob bytes through ATT01 before appending the new v2-only `tool/rich-result`; the durable schema references exact attachments and retains server error state. Provider continuation text, TUI replay/live metadata and native runtime events derive from that durable object under `UntrustedContentSource::Mcp`. V1 rejects the new event kind. The first red test observed the old flattened string. Focused gate: core 64, tools 80, session 133, attachments 5, MCP 285, Agent 136 and TUI 133 all green using only diagnosed exact-target reruns.

**TEL03 is accepted only with explicit product reachability.** Base `heycode-telemetry` retains its structural no-egress proof and local-off remains the sole default. New separate crate/plugin `heycode-telemetry-otlp` / `telemetry-otlp-http` owns restart-applied wire-safe settings, per-batch credential-reference resolution and bounded OTLP/HTTP JSON over the composed HTTP service. It validates response/partial-success/retry/cancellation without returning raw transport text. The CLI factory table exposes the plugin but the built-in profile does not select it. A real User profile disables local-off/enables OTLP and proves service ownership, settings exposure, exact inventory and `/plugins verbose`; composing both fails on the shared service. Base telemetry 119 + OTLP 15 tests and the production-loader profile test are green; no live collector claim is made.

**PMM03 and PZA03 complete fixture/state contracts, not product activation.** PMM03's plan/model/dialect-bound state route retains MiniMax native `<think>`, split reasoning details and complete Messages blocks, requires the opaque thinking signature shown by MiniMax's official Anthropic-compatible response schema, rejects duplicate tool ids/cross-model replay/dialect mixing and passes 50 state fixtures (116 package tests). PZA03 scopes `reasoning_effort` only to GLM-5.2/5.3/5.3-Flash, preserves valid older always-thinking state without inventing that field, and rejects missing replay/response reasoning for compulsory-thinking models; 63 tests and multi-step fixtures pass. Neither crate mounts a production inference plugin or claims a live turn. Z.AI general still cannot send `thinking.clear_thinking=false`; those disclosed activation/wire/live gaps remain separate work under the protocol-vs-reachability law.

**Q07 is `[~]`, not `[x]`, and the reason is recorded** (GOTCHAS #182). The cross-platform workflow is written (`.github/workflows/cross-platform-gates.yml`), but the row's acceptance is "deterministic gates **pass** all three" and they do not: `heycode-extensions` does not compile for Windows, and the workflow has never executed on a runner. Writing the gate and passing the gate are different claims; the owner marked it complete and corrected within the minute, which is the same overclaim rejected from lanes twice the same day (#175).

**The cross-check that found the Windows break is itself a result** (GOTCHAS #183). `cargo check --target x86_64-pc-windows-gnu` caught a let-chain reading `lock.path` where `FileLock` is a unit struct off-unix — dead at runtime, fatal at type-check, and invisible to months of macOS development. Equally recorded: `ring` needs a target C compiler this host lacks, so `heycode-skills`, `heycode-runtime-claude`, `heycode-runtime-codex` and everything downstream of `heycode-http` are **BLOCKED, not passing**, and `-gnu` is not the `-msvc` that CI runs. Sixteen delegated rows landed and were verified independently this stretch — P06, P07, MCP08, MCP09, PAWS02, PAWS03, PGCP02, PGCP03, PGCP04, PZA02, PMM02, PLM02, PLM03, U13 — plus C13 and O08 by the owner. **All three MCP listing families are now live in the panel**: `McpListingSupport::CURRENT.resources` and `.prompts` flipped to `true` (`crates/heycode-tui/src/mcp_panel.rs`), so every zero the panel renders is a zero heycode actually walked to. The two tests whose premise that invalidated were repurposed rather than deleted — one now pins the surviving distinction (a walked zero renders `0`; a never-advertised family renders `NotAdvertised`, still never `0`), the other keeps the `Unsupported` arm exercised for the next listing family that lands. MCP09 also traded robustness for honesty by owner sign-off: a **required** server advertising a listing heycode cannot walk now fails composition, because `McpContributionCounts` is documented complete and cannot express "unknown"; retention means an established connection degrades rather than misreports. PGCP04 leaves Gemini `image_input` **Unknown** — GOTCHAS #121 does not transfer, because OpenRouter's `native_web` is gateway-provided whereas image input is model-intrinsic. `UntrustedContentSource::Mcp` and `ProviderStateKind::GeminiModelContent` were added to `heycode-core`; the second deliberately opens a one-way forward-compat door, since a v2 log carrying it is unreadable by an older heycode (the intended closed-set contract, AGENTS §1). The provider-selection error now distinguishes a genuinely unknown name from one that ships a profile and catalog but has no inference route yet — the old message was false, and the false half was the actionable half (GOTCHAS #169). **Tracker corrections:** four dependency cycles were removed (#157), and PL03's corrected dependencies later closed before its concrete product activation was accepted. **Known host risk (not a code defect):** first execution of a newly written binary can be slow after a rebuild; the current Claude boundary uses a reviewed version interval and the accepted R08 live canary records the installed protocol behavior rather than weakening its timeout contract. **Evidence gaps:** no installed LM Studio/Ollama chat proves PLM05, and POR04/POR05/QLIVE01 await trustworthy OpenRouter credentials. Active platform/approval evidence includes E11, E13 and QSEC01.

**U22 and O05 are accepted as one visible lifecycle vertical.** The TUI registers exact `side-panel:diff|jobs|agents` contributions and persisted Ctrl+B cycles their bounded shared projections in both full and flat modes. Diff rows derive from committed edit/write transcript results, Jobs from the effect-owned registry, and Agents from the live provider/preset catalog without exposing foreign child ids. Background `task` reserves its stable id/token before spawn, returns immediately, owns its `JoinHandle`, propagates cancellation and changes visible state only after the durable FollowUp settlement commits. Append failure rolls the reservation back; Context disposal cancels and aborts every remaining job.

**R05 and R08 are accepted through one generic delegated-subagent adapter plus provider-owned process boundaries.** Fresh one-shot children receive distinct durable heycode logs, link the exact runtime session, consume every event through R02, forward permissions through the parent policy, refuse interactive questions, cancel the runtime and close quiescently. Plan/tool/allow/deny/cancel/final/usage behavior is deterministic; tool-free installed-subscription canaries passed against Codex CLI 0.146.0 and Claude Code 2.1.251 without reading secret values. Codex uses ephemeral thread start. Claude uses the current SDK control initialize and user-frame shapes, disables persistence/history/MCP/Chrome/slash commands, and uses a host-minted session identity.

**QLIVE01 is implemented but remains active with POR04.** The scheduled/manual Ubuntu lane requires a process-scoped OpenRouter key, forces a live `z-ai/glm-5.3-flash` catalog, verifies reasoning/tools evidence, runs a text turn and exactly-once client-tool loop through production composition, checks durable reasoning/tool headers and writes a content-withheld freshness-gated artifact. This host has no process-scoped key and retains the known dummy Keychain shadow, so no authenticated call was made and neither row is promoted. A workflow definition or a skipped artifact is not a passing live canary.

**E09/CMD08/O10–O13/CMD11 are accepted as one operational vertical.** Four
optional effect plugins follow Agent/job ownership: shell/PTY execution shares
the common process tree and publishes only after a source-attributed durable
notice; goals use CAS plus bounded round/wake budgets; plan changes commit at
the next accepted pre-step; replaceable workflows checkpoint/resume; schedules
flush, enqueue-before-dispatch, rearm and exclude inherited fork work. The
default world exposes four services, four commands and seven tools with exact
inventory attribution. Agent 201 plus Session/Exec 251 tests, owned clippy and
root composition/service-key/verbose-inventory gates pass. Config remains v25
because no historical Consumer injects these optional services (GOTCHAS #272).

**MCP15 and MCP11 are accepted at their distinct protocol/product boundaries.**
Official Inspector 2.4.0, invoked with isolated temporary npm/Inspector state,
strict-listed and rich-called the real fixture over stdio and Streamable HTTP.
Production heycode transports passed listings, structured ordered results,
cancellation and body-free hostile failures. A stateful local authorization and
resource server separately proved resource→issuer discovery, CSRF state, S256
PKCE, audience, refresh, protected access and cancellation. Inspector did not
drive browser OAuth, and the local OAuth socket used semantic HTTPS identities
without TLS; those facts are disclosed, not folded into the protocol-matrix
acceptance. MCP11's dynamic HTTP body plus the root-composed session product now
route form/URL elicitation, progress and logging through the exact durable
session/TUI bridge; cancellation retires the owned request with no late reply
(GOTCHAS #273/#279/#293).

**Q10 and Q11 are accepted as harness/evidence contracts, not product-quality
marketing.** Q10 records startup, local TTFT, 1K replay, first flat render and
1K-tool composition through owned black-box processes; five debug samples pass
the explicit CI regression ceilings. Release-reference ceilings remain proposed
until hardware and an accepted baseline are recorded. Q11 runs matched
candidate/reference conditions with deterministic graders, Wilson intervals and
paired bootstrap output. Both fixture agents pass 9/9; the verdict is honestly
`insufficient_evidence` because 9 pairs are below the 30-pair decision floor.
That is a valid statistical report and no live superiority claim. QSEC05
subsequently closes only after repairing the separately found strict-projection
defect and passing its real-binary injection/positive-control gate (GOTCHAS
#274/#275).

**QSEC05 is accepted only after the real product defect and positive control
both moved.** The strict request verifier computed source-wrapped content but
the Tool arm copied the raw durable body into `ChatMessage`; the first product
gate therefore failed for Web, MCP and LSP. A red Rust regression captured all
three sources, the shared sink now consumes the derived content, and an
unlabelled legacy tool-result control remains byte-exact. The rebuilt real
binary passes all three deny cases: each reaches a genuine write request, the
denial settles durably, no marker appears, cleanup completes, and the provider
sees the exact source warning. The separate auto-approval control creates its
isolated marker, so denial cannot pass vacuously. The content-free artifact
validator accepts the result (GOTCHAS #275).

**Q13 is complete; Q12 moves only to active.** The isolated chaos runner passes
composition rollback/LIFO disposal, complete/torn/interrupted session
settlement and read-only repair, quiescent subprocess-tree cancellation, raw
SSE fragmentation and status-authoritative body-free provider failure under an
explicit seed. Running it twice produced byte-identical content-free reports.
Five libFuzzer targets and 20 synthetic credential-free seeds cover session,
config, provider, MCP and the public TUI draw path. The ANSI/OSC seed first
failed because controls reached ratatui cells; Markdown now replaces controls
before parsing/highlighting and preserves visible text/newlines. The complete
local PR smoke passes 128 fixed-seed runs for four parsers, 8 high-cost render
runs, both isolated warnings-denied clippy gates and the chaos matrix. Q12 still
needs an observed hosted/scheduled continuous run; a workflow definition and
short local smoke are not that evidence (GOTCHAS #276).

**X07 is accepted only after the installable extension used the shipping
binary.** `heycode app-server --stdio-v1 --workspace <absolute> [--resume
<uuid>]` composes the normal effect-owned AppServer, defaults noninteractive
workspace authority to restricted, reserves stdout for bounded protocol frames
and shuts Context down after EOF/Ctrl+C/settlement. A real installed
`heycode.heycode-vscode@0.1.0` VSIX opened a native session, admitted a turn,
overlapped cancel, closed, resumed the exact UUID in a fresh host, completed a
healthy turn and disconnected. npm protocol tests pass 7/7 including exact
allow-once/deny correlation; the installed shipping fake emitted no permission
request. Its same-process second-turn `unavailable` observation is the
documented one-response FakeProvider script being exhausted, not evidence of a
native-runtime regression (GOTCHAS #277).

**PLM05, R10 and R12 remain active after safe local evidence stopped at the
right boundary.** The Ollama gate starts/pulls nothing and this host has neither
an executable nor daemon. OpenCode 1.18.21 is hash-bound and completes
initialize/catalog/close, but its installed catalog lacks official
`opencode-go/glm-5.3-flash`, so no model substitution or turn occurs. The
Harness launcher/reviewed artifacts are now hash-bound; the pinned checkout has
no built server/tsx, requires pnpm 11.7.0 while 9.15.0 is installed and has a
materially modified lockfile, so no build mutates it. Focused LM Studio/runtime/
OpenCode/Harness evidence is 149 tests with warnings denied. These observations
strengthen future live gates without satisfying any row's required turn
(GOTCHAS #280).

**O06, O07 and O14 are accepted through real composition.**
Session v2 adds strict `team/change` and `review/change` kinds with closed
projection, v1 refusal and redacted export. The default `teams` service/model
tool owns authority-scoped roster, task DAG, mailbox, recovery and exact
team-state→job→A03 settlement; the default `reviewer` service plus
`/review-runtime` captures an exact tracked patch, runs a selectable delegated
runtime under DenyAll in an isolated checkout, rejects mutation and publishes
only strict structured findings. Agent **211** and Session **173** tests,
warnings-denied clippy, root default composition/service-key/verbose inventory
and CLI no-deps clippy pass. Schema 26 adds optional exact
`subagent.worktree_base`; only its presence registers runtime-specific Codex,
Claude and OpenCode worktree providers. A real temporary Git composition proves
all three exact inventory rows. Absence registers none, and Harness remains
ineligible because it cannot prove permission callbacks (GOTCHAS #281/#282).

**PL09/PL10 have real lower execution evidence but remain active for product
activation.** Exact verified bytes launch through heycode-exec with empty
environment, image lease through exec settlement, bounded length-prefixed
framing, serialized correlation, cancellation/tree reap and atomic six-domain
retirement. Wasmtime 48.0.1/WASIp3 typechecks and runs the WIT subset with
empty-default WASI, explicit preopens/IP endpoints, DNS/write-only refusal and
Store-scoped limits. Exec **88**, extensions **135** and extension-host **13**
tests plus warnings-denied gates pass; root now owns the pinned workspace
dependencies. No current config can safely infer installed entrypoint/runtime/
grants/resources/session or six production adapters, so both rows stay `[~]`.
PL11 remains `[~]` and unexposed while QSEC01 is active (GOTCHAS #284).

**The final parallel wave is integrated without promoting external evidence.**
ATT04 is complete at its intentionally hidden scope: strict PCM-WAV admission,
exact-model/format tri-state adapters, durable input/output association,
cancellation, metadata-only app-server/SDK/TUI replay and raw-byte rejection.
OpenAI and Anthropic now own Settings-derived server-tool policies and matching
effect-owned N01 rows; DeepSeek's Chat versus Anthropic Messages dialect is
schema-28 explicit. AWS live routes and Vertex routes use one shared
caller-cancelled `Provider::prepare_inference` phase after model/N01 selection
and before options/P10/C02/C05, so live evidence or GCP profile resolution
cannot change the already committed target. Maintained Vertex catalogs publish
model facts while account access remains Unknown. PL09/PL10 now have typed code
manifest metadata, explicit managed authority/session/grants/resources, real
six-registry adapters and import-exact Wasmtime evidence; the default root uses
the code-aware factory but grants no code authority. MCP owns and drains
elicitation futures, root mounts the exact Agent/TUI route and shared approval,
and hook output cannot render before a durable bridge. Only concrete O09
handlers and provider-specific bound generations remain active. Platform hardening disables
ambient proxies on DNS-pinned fetches, rejects local public-value URLs and
lossy roots, strengthens Windows path/job evidence, and makes every workflow
invoke the real consolidated test binary. Release smoke now executes the
verified candidate's own installer and exact manager-published stable path.
Authenticated/provider/platform/attestation clauses remain active exactly
where the table says they do.

The raw pending count is deliberately acceptance-granular, not a count of 120 independent features. The reconciled scheduling view is:

| Pending bucket | Count | Meaning |
|---|---:|---|
| Active external evidence/approval | 10 | POR04, POR05, QLIVE01, PLM05, E11, E13, QSEC01, Q07, Q14 and Q15 need a run or owner decision |
| Actionable ship implementation | 8 | POA03, PAN03, PMM04, PZA05, PGCP05, PGCP06, PL09 and O09 retain exact product bridges |
| Actionable ship verification/evidence/docs | 8 | PDS04, PAWS04, PGCP07, R10, Q09, Q12, QSEC03 and DOC02 have implementation but still require their named proof |
| Dependency-blocked | 7 | POR07, PAWS06, Q16, Q18, Q19, DOC06 and DOC07 retain unfinished prerequisite edges |
| Post-beta/optional | 5 | CMD12, R12, PL10, PL11 and Q20 remain explicit P3/1.0 scope |
| **Total unfinished** | **38** | Every active and `[ ]` row is retained; no scope was hidden or deleted |

Execution-mode note: root owns central composition, acceptance and every tracker/law/status document. The user has explicitly authorized separate Codex tasks—not subagents—to implement disjoint crate slices directly in this saved project. Those tasks must not share files, create worktrees/branches/copies or edit central docs/the `heycode-cli` composition root; root reviews their current files and evidence before changing any row. POR01–POR03, N01–N04, WEB01–WEB05, ATT01–ATT03, X02–X06, R04–R06 and R08 are complete. POR04/POR05/QLIVE01 remain active only for trustworthy authenticated/per-call evidence. Work proceeds in dependency-driven vertical batches: provider-by-provider profile→catalog→state→native→live-evidence slices, context/usage/compaction, rich web/attachments, MCP transport/management and plugin lifecycle. U22's former P3 priority inversion is closed together with O05.

MCP11 and MCP13 are now complete: pull-owned dynamic HTTP, exact caller-minted
session routing, the live TUI broker and the shared Agent approval policy prove
elicitation/progress/logging/cancellation and annotation-independent action-time
authorization end to end. PMM04/PZA05 remain active for provider-bound
factories, entitlement/runtime admission and live tool generations. O09 now has
the v2-only durable hook event plus real prompt/subagent/MCP call sites and root
attachment, but remains active until concrete structured handler providers are
composed; no decision is inferred from model prose (GOTCHAS #269/#279/#293).

POA03/PAN03/PDS04/POR05 also have stronger shared/provider boundaries but stay
active. Responses and Messages normalize/replay hosted/server-tool events,
later settlement, citations and pause state; DeepSeek's Anthropic dialect is
guarded with operation credentials; OpenRouter keeps both aggregate usage
shapes without synthetic calls. Root still owes request-specific N01/P10
selection, factories and computer/image/MCP upper bridges, while PDS04/POR05
retain explicit authenticated evidence gaps (GOTCHAS #270).

Priority: P0 blocks all product work · P1 critical path · P2 required for public beta · P3 required for 1.0 or explicitly experimental.

## Milestones

| Milestone | Exit condition |
|---|---|
| M0 Truthful baseline | Stale config/credential issues diagnosed; documentation no longer says engineering |
| M1 Usable shell | First-run, trust, command palette, settings, auth and model selection work |
| M2 Provider kernel | Capability-aware native loop supports Tier 1 API/router/local/cloud providers |
| M3 Native features | Provider-native tools/state/compaction/cache work with portable fallbacks |
| M4 Extension platform | Full MCP and distributable plugin lifecycle work |
| M5 Deep agent | Execution seams, terminals/LSP/jobs/subagents/workflows/hooks are complete |
| M6 Public beta | Cross-platform, secure, benchmarked, migratable release |
| M7 1.0 | Stable contracts, certified matrix, support and deprecation policy |

## Critical path

```text
B01 → B03 → S01 → S04 → U01 → U04 → U10
                 └→ A01 → A04 → U06
P01 → P04 → C01 → C04 → N01
MCP01 → MCP04 → MCP10
Q01 starts immediately and follows every workstream
```

## B — baseline, honesty and migrations

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | B01 | P0 | Add a current baseline composition/live audit test | — | Default and configured worlds report exact plugins, commands, tools and services |
| [x] | B02 | P0 | Change STATUS/FEATURES from “base complete” to staged support claims | — | Every checkmark cites real-composition or live evidence |
| [x] | B03 | P0 | Version configuration documents | B01 | Missing/old/current versions classify deterministically |
| [x] | B04 | P0 | Detect historical setup-generated eight-plugin profile | B03 | Current user config produces a migration preview naming omitted capabilities |
| [x] | B05 | P0 | Migrate generated profile snapshots to built-in profile | B04 | Backup + idempotent migration restores current default capability set |
| [x] | B06 | P0 | Preserve intentional custom profiles | B04 | Non-historical custom list is never rewritten silently |
| [x] | B07 | P0 | Add credential validation state, not presence-only state | A01 | Existing stale OpenRouter key is diagnosed before a normal turn |
| [x] | B08 | P0 | Update retired DeepSeek defaults | PDS01 | Fresh default resolves to a current live-discovered model |
| [x] | B09 | P1 | Remove setup writer's static provider/model assumptions | CAT01 | Setup uses provider/catalog services only |
| [x] | B10 | P1 | Add redacted migration/health JSON output | B03,DOC01 | No secret value appears in snapshot |

## K — plugin kernel and profile loader

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | K01 | P1 | Add plugin descriptors beside existing names | B01 | Every built-in reports id/version/source/contributions |
| [x] | K02 | P1 | Define typed service-key constants | K01 | Duplicate literals and wrong table entries fail tests |
| [x] | K03 | P1 | Roll back earlier plugin effects on composition failure | K01 | Failed Nth plugin disposes N-1…1 in reverse order |
| [x] | K04 | P1 | Add plugin contribution inventory | K01 | `/plugins verbose` can attribute each service/tool/command/panel |
| [x] | K05 | P1 | Add plugin scopes and deterministic precedence | S01,K01 | User/project/local/session/managed resolution tests pass |
| [x] | K06 | P1 | Add profile schema/version/source metadata | B03,K01 | Effective tree shows every layer and source |
| [x] | K07 | P1 | Add named profile file selection | K06 | `--profile` and picker apply same layered profile |
| [x] | K08 | P1 | Add `heycode doctor --composition` | K04,K06 | JSON/human output names dependency and activation failures |
| [x] | K09 | P2 | Add plugin activation transaction and health result | K03,K04 | Partial contributions never publish |
| [x] | K10 | P2 | Add plugin reload generation model | K09 | Successful reload swaps once; failed reload keeps last good |
| [x] | K11 | P2 | Add managed plugin/profile constraints | K05 | Forbidden source/capability cannot activate |
| [x] | K12 | P2 | Add project trust gate before executable contributions | U01,K05 | Project hooks/processes/MCP stay inactive until trust |

## S — settings, credentials, authorization and doctor

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | S01 | P1 | Create layered `settings` Service Definition | K01 | Schema defaults/base/user/project values resolve and freeze |
| [x] | S02 | P1 | Implement file settings provider with atomic writes | S01 | Comments/unknown sections survive supported writes; mode safe |
| [x] | S03 | P1 | Add settings revision/CAS and watchers | S02 | Stale UI write returns conflict, not overwrite |
| [x] | S04 | P1 | Create `credentials` Service Definition with references/records | K01 | Settings can describe configured/source/writable without value |
| [x] | S05 | P1 | Implement environment provider as read-only highest precedence | S04 | Writes under shadowing env fail loud |
| [x] | S06 | P1 | Retire OS keychain provider (2026-09-05 policy) | S04 | Native backend removed; new API secrets use the heycode home file without OS prompts |
| [x] | S07 | P1 | Implement owner-only file fallback and migration | S04 | 0600/0700 enforced; legacy credentials migrate atomically |
| [x] | S08 | P1 | Create authorization flow registry | S04,U02 | One flow per key, cancellation and committed-write proof |
| [x] | S09 | P1 | API-key authorization flow with masked entry | S08 | Live validation classifies unauthorized/host/model/network |
| [x] | S10 | P1 | Command-backed credential provider | S04 | Timeout, empty output, rotation and redaction tests pass |
| [x] | S11 | P1 | Unified doctor registry and result schema | S01,S04,K08 | Plugins contribute checks; human/JSON forms are redacted |
| [x] | S12 | P1 | Credential validation cache with expiry/refresh | S09 | Rotation affects next operation; stale validation is visible |
| [x] | S13 | P2 | Import non-secret config metadata from competitors | K05,S01 | Preview imports MCP/providers/settings, never credentials |
| [x] | S14 | P2 | Settings UI contribution registry | S01,U03 | Provider/plugin settings render from schemas and custom panels |
| [x] | S15 | P2 | Managed settings and secret redaction verifier | S01,S04 | Unprovably safe secret schema fails wire exposure |

## U — TUI and interaction shell

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | U01 | P1 | Workspace trust state and persistence | K12 | Unknown workspace blocks project executable contributions |
| [x] | U02 | P1 | TUI wizard/state-machine service | K01 | Boot renders wizard with no provider and no config |
| [x] | U03 | P1 | UI slot/contribution registry | K04 | Plugin panel/dialog/status contribution disposes cleanly |
| [x] | U04 | P1 | Searchable `/` and Ctrl+P command palette | U03,CMD01 | Empty `/` opens menu; fuzzy filter and source display tested |
| [x] | U05 | P1 | Welcome/status card | U03,S11 | Shows effective runtime/model/permission/workspace/health |
| [x] | U06 | P1 | Authorization wizard pages | U02,S08 | API/subscription/cloud/local method render contracts pass |
| [x] | U07 | P1 | Live model picker | U03,CAT01 | Fuzzy search, filters, badges, refresh and stale state |
| [x] | U08 | P1 | Provider/runtime picker | U03,R01 | Inference and delegated paths are visibly distinct |
| [x] | U09 | P1 | Permission/sandbox picker | U03,E10 | Unsupported backend guarantees cannot be selected |
| [x] | U10 | P1 | First-run orchestration | U01,U02,U06,U07,U09 | Fresh user reaches healthy composer without hand-editing files |
| [x] | U11 | P1 | Immediate/queued/interrupting command behavior | U04,CMD01 | Active-turn behavior is deterministic and narrated |
| [x] | U12 | P1 | MCP panel | U03,MCP10 | Status/auth/tools/resources/prompts/actions work |
| [x] | U13 | P1 | Plugin panel | U03,PL06 | Provenance/permissions/enable/update state work |
| [x] | U14 | P2 | Settings browser | U03,S14 | Schema sections and custom panels update with CAS |
| [x] | U15 | P2 | Session picker/resume/fork/rename/archive | U03,C07 | Filters, lineage and recoverable actions tested |
| [x] | U16 | P2 | Context/usage inspector | U03,C13,TEL01 | Contributor tokens/cache/cost display correctly |
| [x] | U17 | P2 | Transcript provider/native event renderers | U03,C04,N01 | Citations/compaction/provider tools replay correctly |
| [x] | U18 | P2 | Composer steering/follow-up semantics | A05 | Enter/Tab/Esc behavior matches state and is visible |
| [x] | U19 | P2 | Screen-reader/no-alt-screen mode | U03 | Flat output and keyboard paths pass snapshots |
| [x] | U20 | P2 | Terminal capability/theme/keymap/Vim support | U03 | Truecolor/256/dumb/narrow and persisted keymaps pass |
| [x] | U21 | P2 | Virtualized long transcript and render cache | U17 | 100K-event session scroll/render meets performance budget |
| [x] | U22 | P3 | Optional side panels for diff/jobs/agents | U03,O04 | Panels are plugin contributions and keyboard accessible |
| [x] | U23 | P0 | UX repair program (`docs/superpowers/plans/2026-09-02-ux-repair.md`, phases 1–10) | U18,U19,CMD01,S01 | Every phase's failing tests pass; black-box PTY runs recorded per phase; GOTCHAS #309–#321 |
| [x] | U24 | P2 | UX repair follow-ups deferred from U23 | U23 | Deny-with-reason card editor; durable prompt history; live key check for OpenAI/Anthropic/Gemini; bracketed paste in a requested flat frame; concurrent MCP connect and probing; `Tool::effect` replacing the scheduler's name list; durable `retry` row on the request header; on-demand `/mcp` probe; full-screen PTY lane (`scripts/tui_blackbox.py`) |

## CMD — command plane

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | CMD01 | P1 | Extend command descriptor with args/timing/availability/source | K04 | Palette never hardcodes command metadata |
| [x] | CMD02 | P1 | Implement `/connect`, `/logout`, `/provider`, `/model`, `/effort` | U06,U07,U08 | Dialogs and persisted selection work |
| [x] | CMD03 | P1 | Implement `/status`, `/doctor`, `/permissions`, `/sandbox` | S11,U09 | Effective state, not requested state, is reported |
| [x] | CMD04 | P1 | Implement `/mcp`, `/plugins`, `/skills`, `/agents`, `/hooks` | U12,U13 | Each opens its owning capability panel |
| [x] | CMD05 | P1 | Implement `/init` with preview | U04 | Generates useful AGENTS.md without overwriting silently |
| [x] | CMD06 | P2 | Implement session lifecycle commands | U15 | new/resume/fork/rename/archive/delete/export work |
| [x] | CMD07 | P2 | Implement `/context`, `/usage`, `/compact` | U16,C12 | Strategy and context state visible |
| [x] | CMD08 | P2 | Implement `/tasks`, `/ps`, `/stop` | O04,E09 | Background state and cancellation work |
| [x] | CMD09 | P2 | Implement `/diff`, `/review`, `/copy`, `/mention` | U17 | Human plane stays out of model history unless scheduled |
| [x] | CMD10 | P2 | Implement `/theme`, `/keymap`, `/vim`, `/settings` | U14,U20 | UI settings persist transactionally |
| [x] | CMD11 | P2 | Implement `/plan`, `/goal` on durable domains | O10,O11 | Optional inline message is logged by domain owner |
| [ ] | CMD12 | P3 | Implement `/feedback` redacted bundle preview | S11,QSEC01 | No transmission without explicit final approval |

## CAT — model catalogs and capability negotiation

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | CAT01 | P1 | Define provider/model descriptor and unknown semantics | K01 | Capabilities distinguish true/false/unknown |
| [x] | CAT02 | P1 | Catalog registry, TTL cache and single-flight refresh | CAT01 | Concurrent refresh makes one call; stale fallback visible |
| [x] | CAT03 | P1 | Model lifecycle/retirement representation | CAT01 | Retired configured model fails with alternatives |
| [x] | CAT04 | P1 | Catalog persistence separate from user selection | CAT02,S01 | Selection stores id only; cache has revision/timestamp |
| [x] | CAT05 | P1 | Capability-aware model filter API | CAT01 | Tool/image/reasoning/stable filters power UI |
| [x] | CAT06 | P2 | Pricing/performance metadata normalization | CAT01 | Source and timestamp retained; unknown never shown as zero |
| [x] | CAT07 | P2 | Catalog override with provenance | CAT01,S01 | User override visible and cannot silently claim native support |
| [x] | CAT08 | P2 | Catalog conformance fixture format | CAT01,Q01 | Provider fixtures carry source/version/capture metadata |

## P — provider kernel and protocols

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | P01 | P1 | Define `InferenceAdapter` and explicit `ResolvedCall` | CAT01,S04 | Unsupported choice fails before transport |
| [x] | P02 | P1 | Split HTTP/SSE transport from provider semantics | P01 | Fragmentation suite passes across adapters |
| [x] | P03 | P1 | OpenAI Responses protocol adapter | P02 | State, phases, tools, usage and stream normalized |
| [x] | P04 | P1 | OpenAI Chat Completions protocol adapter | P02 | Text/reasoning/multi-tool/usage normalized |
| [x] | P05 | P1 | Anthropic Messages protocol adapter | P02 | Blocks, thinking, server/client tools and pause normalized |
| [x] | P06 | P1 | Gemini GenerateContent protocol adapter | P02 | Parts, function calls, thought signatures and usage normalized |
| [x] | P07 | P1 | Bedrock Converse protocol adapter | P02 | AWS event stream, tools and usage normalized |
| [x] | P08 | P1 | Provider error taxonomy and retry policy | P01 | 4xx/429/5xx/overflow/auth classifications contract-tested |
| [x] | P09 | P1 | Per-operation credential resolution | P01,S04 | Rotated key reaches next request; no cross-route fallback |
| [x] | P10 | P1 | Provider request/response waterfalls | P01 | Current auth/telemetry/native-tool consumers use them |
| [x] | P11 | P2 | Exact/estimated token counting registry | P01 | Measurement advertises confidence/source |
| [x] | P12 | P2 | Rate-limit and cost metadata | P01,CAT06 | `/usage` displays provider facts without guessing |
| [x] | P13 | P2 | WebSocket transport where supported | P03 | HTTP fallback and reconnect metrics tested |

## POR — OpenRouter

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | POR01 | P1 | OpenRouter auth/descriptor plugin | P04,S09 | Invalid current stored key fails connect validation |
| [x] | POR02 | P1 | Live `/models` and single-model catalog | CAT02,POR01 | `z-ai/glm-5.3-flash` metadata matches the current official endpoint fixture |
| [x] | POR03 | P1 | Provider routing policy | POR01 | order/fallback/parameters/ZDR/data policy reach wire |
| [~] | POR04 | P1 | Reasoning/tool capability mapping | POR02 | GLM-5.3-Flash reasoning/tool request succeeds live |
| [~] | POR05 | P2 | `openrouter:web_search` server tool | N01,POR01 | Calls/citations normalize and replay |
| [x] | POR06 | P2 | Explicit request plugin transforms | POR01 | response-healing/PDF/context transform never enable silently |
| [~] | POR07 | P2 | OpenRouter live conformance lane | QLIVE01 | Text/tool/search/routing canary artifacts pass |

## PDS — DeepSeek

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PDS01 | P0 | Current V4 provider/model catalog and migration | CAT03,P04 | Legacy defaults rejected; V4 model selectable |
| [x] | PDS02 | P1 | Thinking toggle/effort resolution | PDS01 | Unsupported sampling removed; high/max mapped |
| [x] | PDS03 | P1 | Reasoning-state preservation across tool turns | C04,PDS02 | Missing state regression produces pre-dispatch failure |
| [~] | PDS04 | P2 | Anthropic-format DeepSeek profile | P05,PDS01 | Tool/reasoning parity fixture and live smoke pass |
| [x] | PDS05 | P3 | Strict tools/JSON/FIM/prefix optional capabilities | PDS01 | Each separate capability has explicit endpoint and tests |

## POA — OpenAI API

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | POA01 | P1 | OpenAI API auth/catalog plugin | P03,CAT02,S09 | Models and account-capable features refresh |
| [x] | POA02 | P1 | Responses state/phase/reasoning preservation | C04,POA01 | Stateless tool loop replays required items |
| [~] | POA03 | P2 | Hosted tool normalization | N01,POA01 | web/file/code/shell/computer/image/MCP events gated by capability |
| [x] | POA04 | P2 | Native `/responses/compact` | C12,POA01 | Opaque checkpoint continues correctly |
| [x] | POA05 | P2 | Prompt cache controls and usage | POA01 | cache options and read/write usage visible |

## PAN — Anthropic API

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PAN01 | P1 | Anthropic auth/catalog plugin | P05,CAT02,S09 | Model list and token count available |
| [x] | PAN02 | P1 | Thinking/interleaved tool state | C04,PAN01 | Required thinking blocks survive tool continuation |
| [~] | PAN03 | P2 | Server tools and pause-turn loop | N01,PAN01 | search/fetch/code/advisor/tool-search/MCP normalized |
| [x] | PAN04 | P2 | Native server compaction | C12,PAN01 | Compaction block persists and continuation passes |
| [x] | PAN05 | P2 | Context editing strategies | C12,PAN01 | Tool/thinking clearing metadata and cache impact recorded |
| [x] | PAN06 | P2 | Prompt caching/token counting | PAN01 | Provider-estimated count and exact cache usage power pressure/usage UI |

## PMM — MiniMax

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PMM01 | P1 | MiniMax provider profiles and distinct credential kinds | P04,P05,S04 | PAYG and Token Plan cannot be confused |
| [x] | PMM02 | P1 | OpenAI/Anthropic model discovery | CAT02,PMM01 | Both list endpoints normalize current models |
| [x] | PMM03 | P1 | Interleaved reasoning/tool state | C04,PMM01 | Complete assistant state replay passes fixtures |
| [~] | PMM04 | P2 | Token Plan MCP bundle | MCP01,PMM01 | web search/image understanding install and policy work |
| [x] | PMM05 | P2 | Coding-plan eligibility/profile | PMM01 | Dedicated endpoint requires explicit eligible selection |

## PZA — Z.AI / GLM

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PZA01 | P1 | General and Coding Plan provider profiles | P04,S04 | Endpoints/credentials are distinct and visible |
| [x] | PZA02 | P1 | Live/maintained GLM catalog | CAT02,PZA01 | Limits/capabilities/retirement normalize |
| [x] | PZA03 | P1 | Thinking/function-call state | C04,PZA01 | Multi-step tool fixtures pass |
| [x] | PZA04 | P2 | Native web search | N01,PZA01 | Sources and result metadata durable |
| [~] | PZA05 | P2 | Coding Plan MCP bundle | MCP01,PZA01 | search/reader/vision/Zread integration works |

## PLM — LM Studio/local

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PLM01 | P1 | LM Studio endpoint/auth/health plugin | P03,P04,P05 | Server/version and compatible protocols detected |
| [x] | PLM02 | P1 | Native model list and capability mapping | CAT02,PLM01 | loaded/downloaded/tool-trained state visible |
| [x] | PLM03 | P1 | Conservative tool-capable route validation | PLM02 | Chat-only model is not offered for agent mode |
| [x] | PLM04 | P2 | Explicit load/unload settings command | U14,PLM02 | User controls context/hardware settings; no surprise load |
| [~] | PLM05 | P2 | Ollama-compatible sibling plugin | P04,CAT02 | Local provider picker and live smoke pass |

## PAWS — Amazon Bedrock

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PAWS01 | P1 | AWS auth/profile/region provider | S08 | API key and SDK chain status validated without secrets |
| [x] | PAWS02 | P1 | Mantle `/models` discovery | CAT02,PAWS01 | Exact accessible models normalized |
| [x] | PAWS03 | P1 | Runtime `ListFoundationModels` discovery | CAT02,PAWS01 | lifecycle/modalities/stream/inference metadata retained |
| [~] | PAWS04 | P1 | Converse adapter profile | P07,PAWS03 | Stream/tool/cache fixture and live smoke pass |
| [x] | PAWS05 | P2 | Mantle Responses/Messages profiles | P03,P05,PAWS02 | Capability difference per endpoint enforced |
| [~] | PAWS06 | P2 | Prompt caching/guardrail/cross-region metadata | PAWS04,P07 | Request and usage/cache facts visible |

## PGCP — Vertex AI

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PGCP01 | P1 | ADC/project/location auth profile | S08 | Account/project/location health checked |
| [x] | PGCP02 | P1 | Gemini catalog/profile | CAT02,PGCP01 | Accessible model capabilities normalized |
| [x] | PGCP03 | P1 | Gemini thought-signature state | C04,P06 | Tool continuation never loses signature |
| [x] | PGCP04 | P2 | Function calling and multimodal input | P06,ATT01 | Tool/image fixtures and live smoke pass |
| [~] | PGCP05 | P2 | Google Search/external grounding | N01,PGCP02 | Grounding citations durable |
| [~] | PGCP06 | P2 | Code execution and context caching | PGCP02 | Native events and cache usage normalized |
| [~] | PGCP07 | P2 | Claude-on-Vertex profile | P05,PGCP01 | Auth/model/tool/thinking live test passes |

## R — delegated agent runtimes

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | R01 | P1 | Define `AgentRuntime` registry and session contract | K01,C01 | Native loop implements same surface |
| [x] | R02 | P1 | Normalize delegated runtime events | R01,C04 | Commentary/final/tool/permission/usage phases replay |
| [x] | R03 | P1 | Codex app-server process/wire plugin | R01,E02 | Pinned/runtime version handshake and quiescent close |
| [x] | R04 | P1 | Codex account/model/capability bridge | R03,U06,U07 | ChatGPT status and model list work without token reads |
| [x] | R05 | P1 | Codex ephemeral delegated subagent | R02,R03,O05 | Live subscription smoke and safe permissions pass |
| [x] | R06 | P2 | Codex primary-runtime session bridge | R04,C07 | start/resume/fork/steer/compact and permission UI pass |
| [x] | R07 | P1 | Claude Agent SDK/CLI process plugin | R01,E02 | Auth status, no-persist query and quiescent close |
| [x] | R08 | P1 | Claude delegated subagent | R02,R07,O05 | Live subscription smoke and plan/permission callbacks pass |
| [x] | R09 | P2 | Claude primary-runtime session bridge | R07,C07 | resume/fork/partial events/compact and questions pass |
| [~] | R10 | P2 | OpenCode server/ACP runtime plugin | R01 | Session/event/model/provider bridge passes Ox live smoke |
| [x] | R11 | P2 | Generic ACP runtime provider | R01,X01 | Interoperability fixtures pass |
| [~] | R12 | P3 | DeepSeek Harness SDK runtime plugin | R01 | Local dsh delegation and lifecycle pass |

## N — provider-native tools and transforms

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | N01 | P1 | Define logical native-tool registry/router | P01,C01 | Selection committed in request header |
| [x] | N02 | P1 | Normalize server-tool call/result/citations | C04,N01 | Replay is provider-correct and UI-safe |
| [x] | N03 | P1 | Portable web search/fetch providers | WEB01,N01 | Native fallback equivalence fixtures pass |
| [x] | N04 | P2 | Native/local policy modes | N01,S01 | prefer/only modes refuse unsupported choices clearly |
| [x] | N05 | P2 | Request transform registry | P10 | OpenRouter plugins and provider transforms are explicit |
| [x] | N06 | P2 | Provider-native tool usage/cost telemetry | TEL01,N02 | `/usage` attributes server/local calls |

## C — sessions, request invariant and context

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | C01 | P1 | Specify and implement session format v2 | B03 | v1 read migration and v2 fixtures pass |
| [x] | C02 | P1 | Add `request/header` and `request/context` events | C01,P01 | Full route/prompt/tool snapshot is durable |
| [x] | C03 | P1 | Add provider-state item event | C01 | Lossless provider-tagged state round trips |
| [x] | C04 | P1 | Extend neutral/provider projections | C02,C03 | Each protocol reconstructs required history |
| [x] | C05 | P1 | Independent request-desync invariant | C04 | Mutated live prompt/tool/route fails before network |
| [x] | C06 | P1 | Inbox splice events and projection | C01 | follow-up/steer/inject accounting survives resume |
| [x] | C07 | P1 | Session query/list/resume/fork lineage | C01 | picker and shared-prefix fork work |
| [x] | C08 | P1 | Crash repair for open calls/steps/turns | C01 | outcomes are unknown/interrupted, never invented success |
| [x] | C09 | P2 | SQLite index as projection, JSONL remains truth | C01 | Rebuild index from logs and compare |
| [x] | C10 | P2 | Session export/redacted support bundle | C07,S15 | Lossless/human/redacted forms pass |
| [x] | C11 | P1 | Token meter exact/estimate envelope pricing | P11,C02 | Prompt/tools/state/attachments included |
| [x] | C12 | P1 | Compaction strategy registry/transactions | C01,C11 | native/portable/prune strategies settle durably |
| [x] | C13 | P2 | `/context` contributor projection | C11 | Context budget explains each contributor |
| [x] | C14 | P2 | Provider switch over opaque-state policy | C03,C12 | portable recompact/fork/cancel options tested |
| [x] | C15 | P2 | 1K-turn compaction/replay stress | C12 | No gap/desync/lost provider state |

## E — execution world

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | E01 | P1 | Define filesystem seam and migrate file tools | K01 | Local provider passes existing behavior/freshness tests |
| [x] | E02 | P1 | Define subprocess/process-tree seam | K01 | Spawn/cancel/kill/wait and env scrub pass |
| [x] | E03 | P1 | Define shell request/spec seam | E02 | Defaults resolve once; nonzero/timeout are results |
| [x] | E04 | P1 | Route every process through sandbox service | E02 | No direct model process spawn bypasses policy |
| [x] | E05 | P1 | Canonical path and symlink policy | E01,E04 | TOCTOU/path escape tests pass |
| [x] | E06 | P2 | Retained-output spill service | E01 | Complete output cap includes wrappers/metadata |
| [x] | E07 | P2 | Persistent terminal/PTY registry and tools | E02 | Owner-scoped sessions, resize/read/write/kill work |
| [x] | E08 | P2 | LSP service, stdio provider and tool | E01,E02 | Definition/references/diagnostics fixture works |
| [x] | E09 | P2 | Background shell/terminal job integration | E03,E07,O04 | Jobs settle and announce once |
| [x] | E10 | P1 | Sandbox policy/backends capability report | E04 | Readonly/workspace/full choices match enforcement |
| [~] | E11 | P1 | Linux Landlock/bwrap runtime CI | E10 | Inside write allowed, outside denied, process tree confined |
| [x] | E12 | P1 | macOS Seatbelt runtime CI | E10 | Path/network/socket matrix passes |
| [~] | E13 | P2 | Windows sandbox/process-tree runtime CI | E10 | Native denial and cleanup matrix passes |

## WEB — portable web and security

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | WEB01 | P1 | Define web search/fetch seam | K01 | Consumer tool independent of provider |
| [x] | WEB02 | P1 | Redirect-aware DNS SSRF guard | WEB01 | Metadata/private/rebind/redirect matrix denied |
| [x] | WEB03 | P1 | HTML/PDF extraction with citations | WEB01,ATT01 | Bounded readable result and source metadata |
| [x] | WEB04 | P2 | Provider registry and domain policy | WEB01 | Search/fetch provider selection visible |
| [x] | WEB05 | P2 | Untrusted-content/prompt-injection annotations | WEB03,C02 | Request and UI mark retrieved content untrusted |

## ATT — attachments and multimodal

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | ATT01 | P1 | Attachment store and durable metadata | C01,E01 | Content-addressed bytes, MIME/size validation |
| [x] | ATT02 | P1 | Image input TUI/CLI/provider projection | ATT01,CAT01 | Capability refusal or successful live image turn |
| [x] | ATT03 | P2 | PDF/document input/extraction policy | ATT01,WEB03 | Native vs extraction path explicit |
| [x] | ATT04 | P3 | Audio input/output groundwork | ATT01 | Hidden experimental descriptor and fixtures |

## MCP — Model Context Protocol

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | MCP01 | P1 | Define MCP registry/snapshot/state model | K01,S01 | Server definitions and generations inspectable |
| [x] | MCP02 | P1 | Refactor existing stdio client under registry | MCP01,E02 | Existing tool round trip and cleanup preserved |
| [x] | MCP03 | P1 | Streamable HTTP transport | MCP01 | Initialize/list/call/cancel fixtures pass |
| [x] | MCP04 | P1 | OAuth PKCE and credential records | MCP03,S08 | Auth/refresh/logout/reauth state pass |
| [x] | MCP05 | P2 | DCR/CIMD/pre-registered clients | MCP04 | Discovery and callback validation suite passes |
| [x] | MCP06 | P1 | Atomic paginated tool generations | MCP02 | List change/conflict/failure keeps correct generation |
| [x] | MCP07 | P1 | Bounded reconnect supervisor | MCP02,MCP03 | Crash-loop exhausts, recovery swaps once |
| [x] | MCP08 | P2 | Resources and subscriptions | MCP03 | List/read/change and UI inspect work |
| [x] | MCP09 | P2 | Prompts and server instructions | MCP03 | Prompt args and instructions available to Consumers |
| [x] | MCP10 | P1 | CLI/TUI management and diagnostics | MCP01,U03 | add/list/auth/test/edit/enable/remove parity |
| [x] | MCP11 | P2 | Elicitation and progress/logging | MCP03,U12 | Correct session/UI routing and cancellation |
| [x] | MCP12 | P2 | Rich result and output-schema bridge | MCP06,ATT01 | Ordered text/link/image/structured results preserved |
| [x] | MCP13 | P2 | Per-server/tool approval and allowlists | MCP06,U09 | Annotations never bypass policy |
| [x] | MCP14 | P2 | Non-secret competitor import | S13,MCP01 | Preview creates unresolved auth references only |
| [x] | MCP15 | P2 | Official inspector and real OAuth server tests | MCP04,MCP12,Q01 | Protocol matrix passes |

## PL — distributable plugins

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | PL01 | P1 | Plugin manifest v1 and validator | K01 | Unknown/incompatible/colliding contributions fail loud |
| [x] | PL02 | P1 | Versioned content-addressed install cache | PL01 | Atomic install and prior-version retention |
| [x] | PL03 | P1 | Declarative skills/commands/agents/hooks/themes/providers | PL01,O08,U20 | Each contribution activates/disposes in real composition |
| [x] | PL04 | P1 | Plugin-bundled MCP | PL01,MCP01 | Relative command/metadata and policy work |
| [x] | PL05 | P1 | Marketplace sources/catalog/signature/checksum | PL02 | Pin/provenance/substitution tests pass |
| [x] | PL06 | P1 | CLI/TUI plugin lifecycle | PL02,U03 | install/enable/disable/update/rollback/remove work |
| [x] | PL07 | P2 | Dependency/conflict/platform resolver | PL01 | Deterministic graph and actionable failures |
| [x] | PL08 | P2 | Managed marketplace/plugin policy | K11,PL05 | Forbidden/unpinned sources cannot install |
| [~] | PL09 | P2 | Out-of-process code plugin protocol | PL03,E02 | Crash/cancel/capability grants remove contributions |
| [~] | PL10 | P3 | WASI Component Model host and WIT v1 | PL09 | Capability isolation and ABI compatibility suite |
| [~] | PL11 | P3 | Curated model-facing plugin inspector | PL06,QSEC01 | Read-only report omits secrets and host internals |

## A — native agent loop

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | A01 | P1 | Make native loop an `AgentRuntime` provider | R01 | Existing send/cancel/session tests pass through registry |
| [x] | A02 | P1 | Add pre-step/request/request-error seams | C02,A01 | Current compaction/context consumers migrate |
| [x] | A03 | P1 | Durable inbox follow-up/steer/inject | C06,A01 | Busy and idle wake rules deterministic |
| [x] | A04 | P1 | Reusable per-turn cancellation owner | A01 | Cancelled turn does not poison next turn |
| [x] | A05 | P1 | TUI steering/follow-up wiring | A03,U03 | Live behavior and transcript sources match |
| [x] | A06 | P1 | Parallel tool scheduler with ordered commits | A01,E01 | Random completion property tests pass |
| [x] | A07 | P1 | Complete error/turn/step closure | A01,C08 | Every provider/tool failure leaves valid log |
| [x] | A08 | P2 | Deferred tools/Code Mode provider | A06,N01 | Large catalog context/perf benchmarks improve |
| [x] | A09 | P2 | Max-turn/budget and loop-hygiene plugins | A02 | Runaway loop stops with durable reason |

## O — orchestration, jobs and automation

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | O01 | P1 | Define subagent provider/continuation contract | R01 | Native providers migrate without behavior loss |
| [x] | O02 | P1 | Fresh/fork/continuable native providers | O01,C07 | Context inheritance and durable child sessions correct |
| [x] | O03 | P1 | Ownership/depth/authority model | O01 | Nested/continuable paths cannot bypass limits |
| [x] | O04 | P1 | Background job registry and control tools | K01,A03 | Settlement/notice/wake budget tests pass |
| [x] | O05 | P1 | Subagent jobs and UI | O01,O04,U22 | Foreground/background result and cancellation work |
| [x] | O06 | P2 | Worktree provider for isolated agents | O01,E01 | Create/cleanup/recovery and Git safety pass |
| [x] | O07 | P3 | Agent teams roster/task DAG/mailbox | O02,O04 | Revision/ownership/recovery stress passes |
| [x] | O08 | P2 | Hook service and command protocol | K01,E03 | Pre/post lifecycle, timeout, trust and disposal work |
| [~] | O09 | P2 | Prompt/subagent/MCP hook providers | O08,O01,MCP01 | Handler result and failure policy tested |
| [x] | O10 | P2 | Goal domain and round driver | C01,A03 | CAS, rearm, round/wake budget and resume pass |
| [x] | O11 | P2 | Plan-mode pending/commit lifecycle | A02,C01 | Mid-turn/off-turn transitions and review gate pass |
| [x] | O12 | P2 | Workflow definition/worker/tool | O04 | Schema, progress, cancellation, checkpoint pass |
| [x] | O13 | P3 | Durable schedules | C07,A03 | flush-before-dispatch, restart, fork semantics pass |
| [x] | O14 | P2 | Review plugin and selectable reviewer runtime | O01,CMD09 | Structured findings, no silent mutation |

## X — ACP, app server and SDK

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | X01 | P1 | Complete ACP permission full loop | A01,U09 | Scripted tool request reaches client and resumes |
| [x] | X02 | P1 | ACP cwd/provider/runtime/model selection | R01,CAT01 | Client workspace and route are effective |
| [x] | X03 | P1 | ACP rich events and cancellation | R02,ATT01 | Tool/media/plan/usage stream and interrupt pass |
| [x] | X04 | P2 | Stable heycode app-server JSON-RPC v1 | R01,C07 | TUI can run as client of local host |
| [x] | X05 | P2 | App-server auth/model/MCP/plugin/settings methods | X04,S08,MCP01,PL01 | Dialog clients need no filesystem knowledge |
| [x] | X06 | P2 | Rust and TypeScript SDK clients | X04 | Start/resume/stream/cancel typed examples pass |
| [x] | X07 | P3 | IDE extension protocol proof | X04,E08 | VS Code proof uses same host/session model |

## TEL — usage, telemetry and diagnostics

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | TEL01 | P1 | Session-local usage/cost/latency projection | P12,C01 | `/usage` works with telemetry disabled |
| [x] | TEL02 | P2 | Telemetry service and local-off provider | K01 | No outbound telemetry by default |
| [x] | TEL03 | P2 | OTEL provider with redaction | TEL02,S15 | Secret canary never exports |
| [x] | TEL04 | P2 | Provider/tool/compaction/cache metrics | TEL02,N06,C12 | Purpose and lineage correctly attributed |
| [x] | TEL05 | P2 | Health history for doctor/support bundle | S11,TEL02 | Bounded retained diagnostics survive restart |

## Q — testing, security, performance and release

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | Q01 | P0 | Create shared real-composition test harness | B01 | All product plugins boot through real loader |
| [x] | Q02 | P0 | Create provider conformance fixture runner | P02 | Fragment/failure matrix reusable by every adapter |
| [x] | Q03 | P1 | Add TUI journey snapshot harness | U03 | Trust/setup/commands/MCP/provider journeys replay |
| [x] | Q04 | P1 | Add session/request replay oracle | C05 | Every native provider fixture proves reconstruction |
| [x] | Q05 | P1 | Add MCP protocol/auth/reconnect lab | MCP01 | Stdio/HTTP/OAuth fixtures share assertions |
| [x] | Q06 | P1 | Add plugin lifecycle/HMR lab | K09,PL01 | Register-dispose-reload property tests pass |
| [~] | Q07 | P1 | Cross-platform CI macOS/Linux/Windows | Q01 | Deterministic gates pass all three |
| [x] | Q08 | P1 | Live test artifact schema and secret redaction | Q02,S15 | Captures metadata/outcome, never credentials/content unless opted |
| [~] | QLIVE01 | P1 | OpenRouter GLM-5.3-Flash continuous development lane | POR02,Q08 | Text/tool/catalog smoke green within freshness window |
| [ ] | Q09 | P2 | Full live provider/runtime canary matrix | Q08 | Each supported route has current artifact |
| [x] | Q10 | P1 | Performance benchmark harness and budgets | U03,P01,C01 | Startup/TTFT/replay/render/tool metrics recorded |
| [x] | Q11 | P2 | Matched-model coding-agent eval suite | A01,R01 | Deterministic success and statistical reports |
| [~] | Q12 | P2 | Fuzz session/config/provider/MCP/render parsers | C01,MCP01 | Corpus and crash-free continuous runs |
| [~] | QSEC01 | P0 | Repository threat model and security policy | — | Assets/trust boundaries/abuse cases reviewed |
| [x] | QSEC02 | P1 | Secret canary suite | S04,E02,C01 | No leak across logs/session/prompt/env/support bundle |
| [~] | QSEC03 | P1 | SSRF/symlink/sandbox escape suites | WEB02,E05,E10 | Platform attack matrix pass |
| [x] | QSEC04 | P1 | MCP/plugin supply-chain and OAuth suites | MCP04,PL05 | Substitution/redirect/annotation attacks denied |
| [x] | QSEC05 | P2 | Prompt-injection handling evals | WEB05,MCP12 | Untrusted content cannot directly authorize actions |
| [x] | Q13 | P2 | Chaos/failure injection framework | K03,C01,E02,P02 | Invariants hold at every settlement boundary |
| [~] | Q14 | P2 | Signed installers and update/rollback | B03 | Fresh install and prior-version rollback pass |
| [~] | Q15 | P2 | Release channel and plugin API compatibility policy | PL01,Q14 | Stable/preview/pinned upgrade behavior documented/tested |
| [~] | Q16 | P2 | Fresh-machine onboarding matrix | U10,Q14 | macOS/Linux/Windows new user completes real turn |
| [x] | Q17 | P2 | Migration matrix over saved config/session versions | B03,C01 | Upgrade is idempotent; downgrade guidance generated |
| [ ] | Q18 | P2 | Redacted support bundle and runbook | CMD12,S11 | User previews exact bundle before any transmission |
| [ ] | Q19 | P2 | Public beta release review | All P0/P1/P2 beta tasks | Ship checklist in QUALITY_AND_RELEASE passes |
| [ ] | Q20 | P3 | 1.0 contract freeze/deprecation review | Q19 | Stable CLI/config/session/plugin API policy published |

## DOC — documentation and product truth

| Status | ID | Pri | Task | Depends | Acceptance |
|---|---|---:|---|---|---|
| [x] | DOC01 | P0 | Link engineering plan from root docs | — | README/STATUS/TASKS/FEATURES point to program |
| [~] | DOC02 | P1 | Add user setup/provider/MCP/plugin guides | U10,MCP10,PL06 | Fresh-machine examples verified |
| [x] | DOC03 | P1 | Generate provider/model capability reference | CAT01 | Reference generated from descriptors, not hand copied |
| [x] | DOC04 | P1 | Generate command/config/plugin references | CMD01,S01,PL01 | Sources and docs freshness-gated |
| [x] | DOC05 | P1 | Architecture and session v2 docs | C01,K01 | Code and diagrams agree |
| [ ] | DOC06 | P2 | Security, troubleshooting and support docs | QSEC01,S11 | Doctor errors link to exact repairs |
| [ ] | DOC07 | P2 | Provider support/last-verified dashboard | Q09 | Claims generated from live artifact metadata |

## Program rules

- Do not start a task whose dependencies are incomplete unless the task explicitly produces a test seam required by those dependencies.
- Keep at most one active task per tightly coupled code area; parallel work must not share files.
- Mark a task complete only after its acceptance condition and relevant global gates pass.
- A provider profile/route support task cannot be complete without current model discovery, credential failure, tool/state and live evidence. A reusable protocol-only adapter may complete on strict conformance fixtures, but remains non-product-accessible until its profile task supplies that evidence.
- A product-visible plugin cannot be complete without default/profile composition coverage and UI/CLI discoverability.
- When a task changes a public contract, update `AGENTS.md`, `FEATURES.md`, `docs/STATUS.md`, `docs/GOTCHAS.md` and the generated/reference docs in the same change.
