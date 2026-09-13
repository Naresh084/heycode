# GOTCHAS — mistakes made & fixed during the build (do not repeat)

Each entry cost real debugging time. Read before writing code in the named area.

## 1. Adding session event kinds
- `SessionEventKind` is a CLOSED serde set with explicit per-variant `#[serde(rename = "plan/mode")]` — `rename_all="kebab-case"` CANNOT produce slash tags. Add: variant + `name()` arm + KNOWN_KINDS + tag-drift test fixture + projection ignore-arm + AGENTS §4 table row.
- **Only TWO of those five are compiler-enforced**: the exhaustive matches in `name()` (`event.rs`) and `derive_messages` (`projection.rs`). `KNOWN_KINDS` is a plain `&[&str]` const and the drift fixture is a hand-written `Vec` — omit either and it compiles, then hard-errors at resume on your OWN logs. The tag-drift test is what catches it; do not skip it.
- Ids inside events are newtypes (`heycode_core::CallId`), serde-`transparent` so the v1 JSONL is byte-identical. Convert to/from bare strings only at the provider boundary (`heycode-agent::mapping`).
- **Sixth point: does this kind's validity depend on its NEIGHBOURS?** A BACKWARD-looking precondition (like `assistant/audio`, which needs a prior request and prior admissions) is decidable at the commit point and belongs in `validate_against_events`. A FORWARD-looking one (like `user/attachments`, which must be followed immediately by `user/message`) is undecidable for a lone append: it must commit inside an atomic batch, and `advance_pairing` in `crates/heycode-session/src/session.rs` must be extended so the reader's whole-log postcondition and the writer's per-batch check stay one piece of code.

## 2. Session locking model
- Session lives behind **`std::sync::Mutex`**, not tokio's. Appends are quick sync fs writes; nothing awaits while held. Using tokio Mutex forced `blocking_lock()` inside sync plugin `apply()` → runtime panic ("Cannot block the current thread from within a runtime"). Lock helper everywhere: `.lock().unwrap_or_else(|e| e.into_inner())`.
- **One WRITER per session log.** A writable handle holds an exclusive advisory lock on the session DIRECTORY (`Session::open_for_writing`, `Session::create*`, `Session::fork`); read-only consumers use `Session::open` and take no lease, so listing never blocks on a live session. Never move that lock onto `session.jsonl` — readers take a blocking shared lock there and `rename`/`archive`/`delete` upgrade it, so an exclusive log lock deadlocks the picker. In-process the lease is shared and the newest holder supersedes older ones (`AppendError::Superseded`); across processes a second writer is refused with `OpenError::AlreadyOpen`.
- **Reserve the lease before replay, publish it only after the handle exists.** `PendingWriterLease` takes the cross-process reservation first, so a contended resume fails with `OpenError::AlreadyOpen` instead of tearing a read of the live writer, and commits the in-process generation only once the handle is constructed. Bumping the generation before `Session::open` returns `Ok` permanently supersedes a still-live writable handle in this process — the exact bug this ordering exists to prevent. Never reserve and publish in one step.

## 3. Waterfall semantics
- `Layer::handle` receives `Next`; calling it delegates, returning WITHOUT it short-circuits deliberately (guards/approval rely on this). Denials are monotonic: later layers must never resurrect an Allow. The seam is ASYNC (approval dialogs await humans).
- Late mounting after publication: `Waterfall::push_shared`, `ToolRegistry::register_shared`, `PromptRegistry::section_shared`, `CommandRegistry::register_shared`. Execution order = early layers then shared in registration order.

## 4. clippy workspace lints are -D (deny)
- `unwrap_used/expect_used/panic/print_stdout/print_stderr/dbg_macro/unimplemented` denied crate-wide INCLUDING tests (`todo` warns). `undocumented_unsafe_blocks` is NOT configured — it is moot because `unsafe_code = "forbid"` in the root `Cargo.toml`. `crates/heycode-cli/src/main.rs` carries a file-wide `#![allow(clippy::print_stdout, clippy::print_stderr)]`: it is the CLI, printing is its job. Convention: every test module opens with `#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`. Production code uses let-else / match / map_err instead. The single sanctioned unsafe (`env::set_var`) was REMOVED entirely — providers take `from_key()`; never reintroduce env mutation.
- `missing_docs` = warn→fix; every pub item needs docs incl. `# Errors` on Results.

## 5. tokio runtime nesting
- Never `block_on`/`Runtime::new()` where a runtime may already be active. MCP plugin solved it by giving EACH connection its own tiny current_thread driver-runtime and hopping via mpsc channels; connect steps run on a PLAIN OS thread driving that runtime (`drive_on_plain_thread`). Same hazard hit: ACP permission forwarder must own `Arc<Acps>` (not borrow) to stay Send.
- crossterm EventStream requires tokio features io-std? No — stdin/stdout handles need `features=["io-std"]` on tokio.

## 6. Serde/config defaults diverge
- `#[serde(default = "f")]` on a FIELD only applies when the PARENT SECTION exists; a wholly-absent section uses `Default::default()`. If they disagree you get two different defaults (web.enabled bug). Rule: manual `impl Default` for section structs, derive only Deserialize.

## 7. schemastery-style option traps (tui-textarea/serde_json args)
- Optional tool args: `.and_then(Value::as_u64).unwrap_or(cfg)` reads CONFIG not hidden literals — config is the single defaulting home.
- Schemastery-like enum-with-default in JSON schemas: emit `"enum"` arrays explicitly.

## 8. DDG parser cursor bug class
- Forward-scanning `find(marker)` then searching FORWARD for earlier attributes (href before class) silently skips results. Fix pattern: locate marker → rfind enclosing `<a` → parse within that segment → advance cursor past ABSOLUTE opening-tag end (compute as anchor_open + inner offsets, NOT relative ones). Always assert multi-result fixtures.

## 9. Publish-at-commit-point ordering
- UI events fire AFTER durable append succeeds. Approval dialogs park INSIDE decide(); resolution flows policy.answer(id,bool). Plan-mode guard reads state that flips only post-append. Breaking this order desyncs TUI vs log.

## 10. FakeProvider script consumption
- One script per `stream()` call, consumed strictly sequentially across parent AND child agents AND titler/summarizer side-calls. Any new automatic provider call (auto-compaction, auto-title) shifts every later script. Tests opt OUT via AgentOptions{auto_title:false} / CompactionPolicy{auto:false}. When adding automatic calls, audit ALL provider-consuming tests.

## 11. ToolCtx Default spread
- ToolCtx has `..Default::default()`-friendly construction; when adding fields prefer builder methods (with_cwd) over new required literals — 12+ call sites otherwise.

## 12. Edition 2024 gotchas
- `env::set_var/remove_var` are UNSAFE (we removed all uses). `gen` keyword reserved. Rustfmt reflows aggressively — run fmt BEFORE clippy -D or formatting diffs mask real lints.

## 13. Windows/CI notes
- pwsh parity family intentionally mirrors bash call-for-call (jscpd-suppressed upstream; here just keep both shells' semantics aligned if adding shell features).
- Timer caps: any ms budget must clamp ≤ MAX_TIMER_DELAY_MS (~24.8 days) and reject 0 where Node/tokio treat 0 specially.

## 14. UI event contract
- `UiEvent::ToolStarted/Finished` carry FULL structured `Value`s; front ends derive previews/tails. Never re-add pre-formatted strings to events — it killed per-tool rendering once.
- Approval ids: `decide()` mints from 0; any hand-built `ApprovalRequested` in tests must use matching ids or `answer()` no-ops and the parked task hangs forever.
- Reasoning blocks collapse on TurnFinished (`done` flag); render respects global `show_reasoning` toggle.

## 15. TurnFinished is a breaking struct literal
- Adding fields to `UiEvent::TurnFinished` breaks every `UiEvent::TurnFinished { reason, usage }` literal across agent/tui/tests (struct literals need ALL fields). Grep `TurnFinished {` in tests+src when extending it — or switch to `#[non_exhaustive]`-style builder if it grows again.
- Resume replay pairs tool cards via `open_tool` id matching (last unmatched call_id wins); multi-tool steps replay correctly only because tool/result follows each call in order.

## 16. Waterfall layers that never delegate
- `PlanGuard` bound its `Next` as `_next` and returned `Ok(())` on EVERY path, not just the deny path — so every layer registered after it on `seam/pre_tool` was silently skipped for every tool call. It went unnoticed for one reason only: `plan_plugin` was never composed, so the guard never ran in the shipping binary.
- Rule: a layer returns without calling `next` ONLY when it short-circuits deliberately (a denial). Every other path ends in `next.run(input).await`. When you write a layer, write the paired test too: register a tally layer AFTER yours and assert it still runs on the allow path (`crates/heycode-agent/tests/plan.rs`).
- Corollary: a plugin that is never composed has no test coverage that means anything. If a plugin is documented as shipped, a composition test must assert its service key is present in a default world (`crates/heycode-cli/tests/composition.rs`).

## 17. `let _x = …` silently discards work
- `/skills` built its entire listing into `let _text = …` and returned `Ok(())`. The command was a no-op for its whole life, and nothing caught it because no test asserted on its OUTPUT — only on discovery.
- `let _ = cwd;` in the ACP `session/new` handler did the same to the client's workspace root: read from the request, then dropped, so every relative tool path resolved against the server's cwd.
- Rule: `let _ =` is for genuinely ignorable results (a `send` on a closed channel). Discarding a value you just COMPUTED is a bug. Commands report through `agent.ui().emit(UiEvent::Info { .. })`; test the emitted event, not the return code.

## 18. serde field defaults vs section `Default` (the sequel to #6)
- #6 named the trap but three fields still had it: `[compaction] threshold_ratio` / `context_window` and `[subagent] max_depth` used `#[serde(default)]` on the FIELD, so a TOML that declared `[compaction]` without `threshold_ratio` got **0.0, not 0.8** — auto-compaction then fires on every single turn. `LlmSection`/`ToolsSection` had the mirror problem: a partial `[llm]` section failed to parse outright because `model` had no default at all.
- Fix applied: `#[serde(default)]` on the CONTAINER (not the field) for every section that owns an `impl Default`. Missing fields then come from that one `Default`, which stays the single source of truth. Per-field `default = "…"` helpers were removed as redundant.
- Same class, different file: `Config::defaults()` used to hardcode a `profile.plugins` list of 8 names. Once composition actually read it (see #19) that stale list would have silently dropped skills/mcp/subagent/plan/sandbox. Defaults now leave it EMPTY, meaning "the composition root's built-in profile".

## 19. Composition is config-driven — the factory table is in the BIN
- `[profile] plugins` is resolved through `PluginFactories` (`heycode-config` owns the type; `heycode-cli::compose_world` owns the table, because only the bin may know every crate). An unknown name fails loud with the available list.
- Factories are `FnOnce` so a factory can MOVE captured config/providers/paths into the plugin it builds. Register your plugin in `compose_world` AND add it to `default_profile`'s `ORDER`, or it will never compose when no profile is configured.
- Ordering constraints live in `default_profile`: `tools`/`prompt`/`commands` precede everything that registers into them; `plan` follows `commands` + `approval`; `agent` follows `agent-options`; `tui` is last.

## 20. Read-before-edit is a FRESHNESS check, not a path check
- `ObservationLog` stored a bare `HashSet<PathBuf>`, so any prior read authorized an edit forever. A file rewritten under the model was edited against content the model had never seen — the regression test shows `edit` happily rewriting `hello mars` after the model had only ever read `hello world`.
- It now stores `(mtime, len)` per canonical path. `contains()` = "ever read"; `is_fresh()` = "read AND unchanged". `edit` checks both so the two failure modes get different model-facing messages ("Read X before editing" vs "X changed on disk since you read it — re-read it").
- `write` still marks AFTER writing, deliberately: the stamp must describe what was just written.

## 21. `cfg(target_os)` islands hide compile errors
- `LandlockSandbox::confine` bound its parameter as `_argv` while the `#[cfg(target_os = "linux")]` branch called `launcher_argv(&rules, argv)` — a use of an out-of-scope name that macOS CI could not see, and the one shape test early-returned off Linux, so nothing caught it.
- Fix: `launcher_argv` is pure argv construction and now lives OUTSIDE the cfg island, compiled and tested on every host; only `apply_and_exec` (which needs the `landlock` crate) stays Linux-gated. `LandlockSandbox::new()` is what refuses non-Linux.
- Rule: keep the cfg island as small as the syscall that needs it. If a test starts with `if !cfg!(target_os = "…") { return; }`, ask what it is no longer verifying.

## 22. Protocol compatibility is not provider capability parity
- “OpenAI-compatible” proves only that an endpoint resembles one request family. It does not prove reasoning-state replay, tool semantics, native server tools, structured output, token counting, prompt caching, context editing, compaction, error categories or model discovery.
- Concrete examples found during the engineering research: DeepSeek thinking tool turns require `reasoning_content`; MiniMax requires the complete assistant/reasoning state; Gemini requires thought signatures; OpenAI and Anthropic have different native compaction items; Bedrock exposes several runtime families.
- Rule: each provider/model publishes explicit capabilities and owns a `resolve(draft) -> wire call` step. Unknown stays unknown; unsupported combinations fail before network I/O. See [engineering/PROVIDERS.md](engineering/PROVIDERS.md).

## 23. Generated profile snapshots become capability freezes
- An earlier setup wizard wrote the then-current default plugin list into `[profile].plugins`. After new plugins were added, that saved list remained authoritative and silently omitted skills, MCP, subagents, plan and sandbox from the live TUI.
- The setup writer no longer emits a default profile list, but existing generated snapshots remain in user homes. A code fix does not repair durable state by itself.
- Rule: version persisted configuration, recognize historical generated snapshots, preview migrations and convert them to “use built-in profile.” Preserve intentional custom profiles. Add a migration fixture every time generated config semantics change.

## 24. Credential presence is not authentication
- `provider_key_present` proved that a non-empty key existed, so startup entered a normal chat. The first OpenRouter request then failed HTTP 401 while the same provider worked through OpenCode's separately managed credential.
- Shape validation and file mode are necessary but not sufficient. A credential may be revoked, expired, for the wrong host/account, or shadowed by a different source.
- Rule: authorization commits a credential only after a provider-specific validation where possible, stores no secret in diagnostics, records source and validation time, and exposes repair through setup and `doctor`. Configured-but-invalid fails a live test; it is never treated as absent or healthy. See [engineering/ARCHITECTURE.md](engineering/ARCHITECTURE.md#settings-and-credentials).

## 25. A migration fingerprint is unsafe without source provenance
- The historical eight-plugin list is recognizable, but a user could intentionally write the same list in a project or explicit config. Matching bytes alone does not authorize rewriting it. The old setup wizard is known to have persisted only `$HEYCODE_HOME/config.toml`, so automatic profile removal is limited to that discovered source; explicit/project plans remain pending.
- Safe apply is a transaction: canonicalize the destination, retain previewed bytes privately, compare before write, create a byte-exact non-overwriting backup, compare again, then atomically replace. A stale plan or conflicting backup fails loud. Reapplying an already committed plan is a no-op.
- The config crate owns historical fingerprints, while the composition root passes the current built-in order. Copying the current plugin list into migration code would create the next stale snapshot.

## 26. Requested factory names are not sufficient runtime identity
- The profile asks for factory row `approval`, but ask mode applies plugin variant `approval-ask`; resume similarly applies `session-resume` through the `session` factory. Auditing only requested profile strings describes intent, not the live implementation.
- During the name-to-descriptor transition, `descriptor.id` and `name()` are two identity fields. Composition must compare them before apply and fail loud on drift. Descriptors enter `Context` only after apply succeeds, alongside the applied-name audit.
- Broad descriptor contributions state capability families, not exact ownership. Do not use them to claim which particular tool/command/service was registered; that requires the effect-backed contribution inventory in `K04`.

## 27. A typed value behind a string key is only half type-safe
- Before `ServiceKey`, every `provide/get/inject` call repeated literals such as `"agent-options"` and `"approval-interactive"`. The value was statically typed, but a typo in the key compiled and became `None` or an unsatisfied dependency at runtime.
- Every service-definition owner now exports one typed `SERVICE_*` constant; seams follow the same rule. Context operations and `Plugin::inject` accept `ServiceKey`, so consumers cannot invent a near-miss string. The CLI registry pins all names and rejects duplicates in tests.
- `ServiceKey` does not encode the Rust value type. A wrong `T` still returns `None` by contract. Do not claim full typed-DI semantics; `K04/K08` add ownership/type diagnostics without turning core into a framework.

## 28. Dropping a failed composition is not effect rollback
- `Context` stores disposer closures; dropping the vector drops those closures without invoking them. Before K03, `compose()` used `?`/early returns, so any process/listener/temp resource registered by plugins 1…N stayed live when plugin N failed.
- Every failure branch now transfers the context and original error to one abort function, calls idempotent `shutdown()`, then returns the unchanged error. This includes effects a plugin registered before its own `apply()` failed.
- Do not reintroduce `?` inside the composition loop unless the error is wrapped through the rollback path. A load failure is a transaction abort, not ordinary stack cleanup.

## 29. A settings registration is a lifecycle effect, not a map insert
- The first S01 draft returned an owner handle and relied on every consumer to remember `ctx.effect(move || drop(handle))`. One forgotten move would leave a namespace registered after its plugin disappeared and would bypass K03 rollback.
- `SettingsService::register` now requires the live `Context`, installs unregister itself, and returns only the immutable snapshot. Duplicate/validation failures publish nothing; shutdown and failed composition remove successful registrations LIFO.
- Schema JSON is presentation metadata, not automatic validation. The owner validator is authoritative. Layers are detached objects and snapshots expose only shared reads; writes/revisions/watchers belong to providers (`S02/S03`), not the Service Definition bootstrap.

## 30. Permission repair after atomic rename is already too late
- The first S02 writer atomically committed content, then called `chmod(0600)`. If chmod failed, disk held the new document while `replace_user` returned a provider error and correctly refused to publish the candidate—durable and in-memory state diverged.
- On Unix, configure the atomic temporary file with `preserve_mode(false)` and `mode(0600)` before writing. Rename then commits content and safe mode as one visible replacement; no fallible security step follows the commit point.
- Format preservation applies outside the namespace being wholesale-replaced. Replacing one section deliberately owns that section; comments/unknown tables/namespaces elsewhere must survive byte-semantically. Path mutation for callers holding incomplete/redacted views lands with the settings UI/redaction work.

## 31. Filesystem watcher paths need canonical identity
- On macOS, `tempfile` produced `/var/folders/...` while FSEvents/notify reported `/private/var/folders/...`. Exact target comparison silently ignored every real event and the watcher test timed out.
- Canonicalize existing targets; for a missing file, canonicalize its parent and reattach the filename. Watch the canonical parent and accept the target, parent, or same-parent temporary path because atomic replace events may name the hidden temporary file.
- Debounce must have a hard deadline. Draining “until quiet” can starve forever in a noisy directory; S03 caps the burst at 250 ms, then reloads one complete generation.

## 32. Serialized watcher callbacks need an explicit reentrancy rule
- Holding the operation lane through callbacks preserves commit order, but a callback that synchronously calls `replace_user` or `publish_documents` would wait on its own non-reentrant mutex forever.
- S03 marks callback execution thread-locally and returns `ReentrantWrite` before lock acquisition. Callbacks may read the published snapshot or schedule a later write; they may not recursively enter the commit lane.
- Provider generation publication validates every namespace first. A single invalid external section keeps the whole last-good generation, so watchers never observe a partially advanced document.

## 33. Credential inspection must not be secret resolution
- A UI/doctor needs configured/source/writable/validation metadata, but calling `resolve` to obtain it unnecessarily materializes the secret and tempts logging/serialization. S04 gives providers a separate safe `inspect` contract; descriptors contain no value field.
- Secret values use `CredentialSecret(secrecy::SecretString)`: no serde/display/clone surface from heycode, redacted debug, explicit `expose()` only at the operation that needs it, zeroize on drop.
- Provider precedence owns write truth. If a configured high-precedence environment provider is read-only, a writable lower file/keychain provider does not make the active reference writable. Report the shadow rather than silently writing somewhere the next read will ignore.
- S05 implements this rule: `credentials-env` is precedence 0, blank is absent, and `CredentialsService::write` stops with `ShadowedReadOnly` before invoking any fallback.

## 34. Keychain “unavailable” and “failed” are different states
Historical implementation, superseded by the no-OS-store policy in #326.
- A default keychain provider must not make the whole product unload on a host with no supported/default store; that state is unconfigured + non-writable so the owner-only file provider can take over. A locked, malformed, ambiguous or operationally failing store is different and stays loud.
- keyring 4.1.6 exposes no metadata-only existence query. The native backend's `contains` must retrieve a temporary password; zeroize it immediately and return only the boolean. The public credential `inspect` surface still receives no value.
- Never use real keychain mutation in deterministic tests. Inject a backend for write/read/delete contracts; gate native tests and use a deliberately nonexistent account for read-only initialization proof.

## 35. Credential migration removes legacy only after two durable proofs
- Writing the new store is not enough: recovery also needs the exact old bytes. S07 validates/merges, atomically writes new, atomically creates or verifies the byte-exact backup, and only then removes the legacy active file. Conflicting old/new values or backup bytes leave legacy untouched.
- The credential root and store modes are part of the transaction (`0700`/`0600`). As with settings, set mode on the atomic temporary file; do not chmod after commit. Reject symlinks before reading or replacing.
- `Zeroizing<String>` works for raw/serialized buffers, but zeroize does not wipe a `BTreeMap<String,String>` as a unit. Use a drop guard that zeroizes every value; do not mistake a wrapper's desired semantics for an implemented trait.

## 36. First-run UI must compose before a real provider can
- The old startup checked credentials before composition, so “show integrated setup when no credential exists” was impossible: the binary asked a line prompt or failed before the TUI service existed.
- Interactive startup now composes a disconnected provider only when onboarding is active. The composer is blocked, and accidental streaming returns actionable failure. Headless/ACP never get the placeholder.
- Wizard state/actions/outcomes belong to `heycode-onboarding`; the TUI renders snapshots. Do not put provider lists, credential writes or model defaults in `render.rs`. Also route global keys explicitly—an early onboarding event branch initially swallowed Ctrl+C until its double-press contract was restored.

## 37. An authorization grant is not authorization success
- A flow may collect/exchange a secret, but only the credential registry knows provider precedence and durable write semantics. Flows return `AuthorizationGrant`; the registry writes, reads back a safe descriptor, verifies the writer remains authoritative, then emits a receipt.
- Check the same cancellation token both before invoking the flow and after it returns, immediately before commit. A flow can cancel while finishing; that grant must drop/zeroize without touching a provider.
- Receipts and errors are safe metadata only. Never include a grant, raw provider response or credential value in debug/serde/UI state.

## 38. Credential validation errors are a closed safe taxonomy
- Raw provider bodies can echo operational/account data and vary arbitrarily. S09 maps only status/transport/catalog facts to `unauthorized|host|model|network|cancelled`; bodies never enter errors, logs or receipts.
- Authenticated key validation and model entitlement are separate checks. A 200 key endpoint does not prove the configured model exists; missing catalog id is `model`, not unauthorized.
- A masked-input interface is not permission to fall back to stdin. Until U06 supplies the dialog, mounted flows fail with a safe input-UI error; they never echo or opportunistically store the key.

## 39. Masked rendering is not enough; secret state must stay off shared planes
- A password glyph prevents shoulder-surfing but does not prevent transcript/EventBus/debug/session leakage. Secret prompt notifications carry only id, prompt, reference/kind and `masked`; answer delivery is a oneshot `CredentialSecret` outside model/session/UI events.
- TUI raw input is a private capped String, zeroized on cancel/drop, and moved directly into `CredentialSecret` on submit. Never put it in `TextArea`, `Item`, `UiEvent`, onboarding snapshots or error text.
- Authorization method rows come from flow descriptors. Do not hardcode DeepSeek/OpenRouter in `render.rs`; connector plugins own labels/method/query, and runtime-class filtering is a pure catalog projection.

## 40. Validation cache identity must include the secret without exposing it
- Key/reference/provider alone cannot detect external rotation: the same reference can resolve different bytes while an old Valid record remains. Cache an internal SHA-256 fingerprint alongside validation and compare on the next resolve; never serialize/display it.
- Expiry is projected as Stale, not Invalid or Unknown. Rotation/mutation/provider disposal clears to Unknown. Only a new live check may produce Valid again.
- Existing credentials need preflight before a normal composer/turn. Presence and a migrated file mode prove neither authorization nor model entitlement; headless must fail before dispatch, while interactive enters repair onboarding.

## 41. Unknown capability is not false—and definitely not true
- Before a live catalog/adapter proves a field, both “unsupported” and “supported” are claims. CAT01 uses a tri-state whose Unknown converts to `None`; `is_supported()` is true only for explicit Supported.
- Protocol compatibility is descriptor evidence, not model capability evidence. DeepSeek/OpenRouter may declare OpenAI Chat Completions while tools/reasoning/native web/compaction remain Unknown.
- Unknown model ids keep their identity and unknown limits. Do not fill context/output windows from convenient defaults; CAT02/CAT03 own evidence and lifecycle.

## 42. The first catalog waiter must not own a shared refresh
- A naive single-flight implementation runs the provider future inside the first caller. If that caller closes a picker or cancels, every concurrent consumer loses the refresh and the flight can remain unresolved.
- CAT02 separates caller wait tokens from the registry-owned source token. Cancelling one wait ends only that wait; provider disposal cancels and settles the shared flight. The operation has a supervised abort handle as a last-resort teardown path.
- Failed, panicking or structurally invalid sources never replace last-good. Ordinary reads expose stale fallback plus the exact safe error; forced refresh and cancellation fail visibly. Never turn cache availability into a false health claim.

## 43. Cached deprecation state can cross its own deadline
- A catalog may say Deprecated with a future retirement instant, then remain inside TTL while wall time passes that instant. Trusting only the cached enum would dispatch a model the same descriptor now proves retired.
- CAT03 resolves lifecycle at an explicit Unix-millisecond instant: a reached retirement deadline becomes effectively Retired without mutating the immutable snapshot. Retired ids and aliases fail with bounded canonical alternatives; future deprecation remains selectable with a visible warning.
- “Descriptor lifecycle Unknown” is not “configured id absent from a complete catalog.” The former lacks evidence and remains selectable; the latter fails. Provider aliases must resolve to one canonical descriptor, and alias/id collisions invalidate the whole candidate generation.

## 44. A cache write is part of catalog publication, not an afterthought
- Publishing a live catalog in memory and then attempting persistence creates two authoritative “last good” generations when the write fails. CAT04 serializes whole-generation commits, atomically persists first, and advances in-memory revision/cache only after the durable provider returns success.
- User selection and discovered metadata have different ownership. `[llm]` keeps only provider/model ids; schema-v1 `$HEYCODE_HOME/cache/models.json` stores safe provider/model generations with revision and timestamp. Never copy a descriptor into settings or treat a cached row as user intent.
- Future schemas may add fields. Probe `schema_version` before strict v1 decoding so a newer document fails as “newer schema” instead of fake corruption. Atomic cancellation is honored before commit; once rename commits, success is published rather than pretending the durable operation rolled back.

## 45. A picker filter is an evidence predicate, not a truthiness test
- Filtering `tools=true` with `support != Unsupported` quietly includes Unknown and tells users an unproven feature works. CAT05 exposes `Any | Supported | Unsupported | Unknown` predicates and compares the exact tri-state.
- `Stable` and `Selectable` are also different: preview, unknown and pre-deadline deprecated models may be selectable without being stable. Retired rows remain queryable for diagnostics but the selectable filter excludes them.
- Lifecycle filters take the same explicit comparison instant as selection. Do not precompute “retired” into mutable cache state or let two UI filters observe different hidden clocks.

## 46. Advisory discovery and authoritative request resolution are different checks
- An unlisted exact model may still be accepted by its adapter, so catalog absence cannot become a universal dispatch whitelist. Conversely, a catalog row marked Unknown cannot authorize requested tools/reasoning/images/native features.
- P01 resolves one `RequestDraft` with one adapter-owned exact-route `ResolveSpec`. Unsupported and Unproven are distinct pre-transport errors; effort ids are checked exactly and never clamped. Adapter defaults are marked so the eventual request header can explain where effective values came from.
- `ResolvedCall` has private fields, is not Clone, and is consumed by `InferenceAdapter::stream`; this binds validated route/defaults to one dispatch by ownership. Do not bridge it through the legacy provider path until every represented field is translated or explicitly rejected—silent field dropping defeats the seam.

## 47. SSE framing is transport; `data` meaning is protocol
- The old parser decoded every `data:` line as OpenAI JSON, so it conflated network reads, SSE fields and provider semantics. That breaks multi-`data:` events, risks split UTF-8 handling and makes every new protocol duplicate transport code.
- P02's `heycode-http` incrementally frames arbitrary byte splits, CR/LF/CRLF, BOM, comments, id/retry and newline-joined data. It validates `text/event-stream`, bounds events/error tails and owns cancellation. It never recognizes `[DONE]` or JSON.
- Chat Completions owns `[DONE]`, deltas, reasoning, tools, usage and finish ordering after receiving `SseEvent`. Keep these layers separate: a framing error is terminal transport failure; a malformed provider data event is protocol failure. Header-bearing request types deliberately have no Debug implementation.

## 48. Responses continuation state is an ordered item stream, not “extra metadata”
- Official Responses stateless/ZDR guidance requires replaying prior user inputs and returned output items; current models may also require encrypted reasoning and the output item's `phase`. Separate `messages` and `provider_state` vectors cannot preserve chronology.
- P03 uses one ordered `InferenceInput::{Message,ProviderState}` list. Completed output items are retained losslessly with provider/model/protocol/schema identity. Function calls keep `call_id`; delta concatenation must equal the completed arguments exactly.
- Responses event `sequence_number`, response id, output index/id/type, start/done settlement and terminal status are invariants. EOF, gaps/reordering, unfinished items and `response.failed` are errors—not a synthetic Stop. Terminal usage remains immediately before Finish. Do not mount this path into the v1 agent loop until C01 can durably log every state item it may replay.

## 49. “OpenAI-compatible” reasoning is not one request field
- Chat-compatible gateways can emit the same `reasoning_content` delta while accepting no effort field, a scalar `reasoning_effort`, or an object such as `reasoning.effort`. Hardcoding one dialect in the shared protocol adapter silently breaks another provider.
- P04 makes request reasoning dialect explicit in `ChatReasoningWire`; advertising effort ids with no dialect fails adapter construction. Provider profiles own the mapping. Stream parsing still preserves reasoning independently of request controls.
- Chat tool calls are fragments keyed by tool index inside one assistant choice. The first fragment must establish stable call id/name; later identity changes, nonzero choice index, late deltas, duplicate finish/usage and EOF before finish are errors. The completed assistant message—including reasoning and fully reassembled parallel tool calls—is one lossless replay state item.

## 50. Append-only format migration means a versioned prefix, not a rewrite
- Silently rewriting a v1 session on resume violates append-only recovery and can destroy the only evidence needed to debug migration. C01 retains every original byte and source `event.v`; the next committed line uses v2.
- A valid mixed log is therefore v1* followed by v2*. A v1 line after any v2 line is a version regression and fails. Versions outside the exported readable range fail with the actual min/max, not a hardcoded diagnostic.
- Kind gates are version-specific even while v1/v2 currently share the same 12 kinds. C02/C03 extend `KNOWN_KINDS_V2` only. Reusing the v2 list for a v1 envelope would let a newly added payload masquerade as historical data and bypass its migration decision.

## 51. A prompt hash without the prompt is not reconstructability
- Logging only a hash can detect some drift but cannot rebuild what the model saw; logging only text makes silent mutation harder to diagnose. C02 stores the exact rendered system text and its lowercase SHA-256, then recomputes on append/read.
- `request/header` carries canonical provider/model/protocol/target, secret-free auth class/reference, complete tool schemas and explicit options/purpose. `request/context` carries capacity/output and paired catalog revision/timestamp under the same `RequestId`.
- These kinds are v2-only and legacy message projection ignores them deliberately. Do not claim the live invariant yet: C04 must reconstruct protocol input and C05 must independently compare it with `ResolvedCall` before dispatch. Never put credential values or auth headers in the snapshot.

## 52. Updating two nearly identical known-kind lists is a migration trap
- The first C03 insertion mechanically matched `assistant/message` in `KNOWN_KINDS_V1`, not v2. The new v1-rejection test failed immediately; without it, historical envelopes could claim a provider-state kind that never existed in v1.
- Keep v1 frozen and edit v2 with version-named context, then run a raw v1 envelope rejection fixture for every v2-only kind. Similar-looking constants are not permission to use a broad search/replace.
- Provider state itself belongs in core because LLM emits it and session persists it without either depending upward. Kind/protocol/schema/data compatibility is validated before append and after read; Responses items cannot be mislabeled as Chat assistant messages.

## 53. Provider state supplements history until it proves it replaces the assistant
- Blindly emitting both lossless provider state and the generic `assistant/message` duplicates output/tool calls. Blindly suppressing the generic message when only a reasoning item survived a crash loses the visible answer.
- C04 suppresses the neutral assistant only when same-route state contains a complete Chat assistant message or a Responses message/function-call item. Reasoning-only state remains additive and neutral fallback stays.
- State must match its producing header's provider/model/protocol, turn/step and contiguous output index. For a different target route, opaque state is excluded and the neutral assistant fallback is used; C14 now requires explicit portable/fork/cancel policy before persisted route mutation. Compaction shadowing applies before substitution.

## 54. Comparing a live request to the snapshot it just created is not independent
- The safety check must re-project committed log events, then compare that detached value with the still-held `ResolvedCall`. Comparing two live structs or reconstructing both sides from the adapter cannot detect prompt/tool/input mutation.
- C05 maps the live call to C02 snapshots for commit, but verification accepts `ProjectedRequest` from session and compares provider/model/protocol/target/auth, system+hash, tools, all options/default provenance, context/catalog evidence and ordered inputs.
- Success yields `VerifiedResolvedCall<'adapter>` bound to the exact adapter instance and consumed on dispatch. Prompt/tool/route/input/context/default drift consumes the unverified call and transport count stays zero. The old compatibility loop does not claim this invariant; new provider-loop activation must use the verified gate.

## 55. A live model list is not a retirement ledger
- Provider `/models` endpoints normally describe ids available now. Once an old id disappears, that response alone cannot distinguish retirement from malformed omission or tell a configured user what replaces it. PDS01 merges the validated DeepSeek generation with maintained, dated tombstones for `deepseek-chat` and `deepseek-reasoner`; resolution can therefore reject them with a current V4 alternative.
- Discovery presence proves identity/availability, not every capability or lifecycle label. Normalize only separately evidenced fields. DeepSeek V4 gets documented limits, Preview state and tool/reasoning/prompt-cache support; unrelated fields and unknown live ids remain Unknown rather than inheriting Chat-protocol assumptions.
- Resolve credentials at each refresh, not when the plugin composes, so rotation is visible. The shared buffered HTTP layer returns status/body to the provider plugin for classification, but provider errors must use stable body-free messages. A 401/403 is unauthorized, 429/5xx unavailable, and malformed JSON/rows reject the complete candidate generation.

## 56. A provider default is a versioned cross-crate product contract
- Changing only `Config::default()` leaves the provider constructor and setup picker emitting the retired id; changing only the provider leaves fresh config stale. B08 pins the same V4 Flash id across config, `DeepSeekProvider`, setup output and the live-catalog selection test. The CLI cross-crate test is the drift detector until B09 removes setup's static provider table.
- An old default string in a file is not enough proof to overwrite user intent. Schema-v2 migration changes `deepseek-chat` only when the older document matches the setup-generated root/LLM/tool fingerprint. A customized v1 document, `deepseek-reasoner`, an OpenRouter route and a current-schema explicit pin retain their exact model for visible catalog rejection/advice.
- Model replacement is a real migration: preview a typed semantic change, preserve value decoration/comments, create the byte-exact versioned backup, source-CAS check, atomically commit, and make the second run a no-op. Bumping a compiled default without bumping the persisted schema would give old home configs no safe upgrade path.

## 57. Provider wire omission must happen during resolution, not only serialization
- DeepSeek thinking ignores unsupported sampling. Removing `temperature` only while building JSON left `ResolvedCall` and its durable C02 snapshot claiming a value the model never saw. PDS02 normalizes it to `None` in the adapter's explicit `resolve` phase, then serialization independently enforces the omission. Disabled thinking retains the caller's temperature.
- Thinking compatibility is route data, never `if provider == "deepseek"` in shared Chat logic. `ChatThinkingConfig` owns the disabled id, complete canonical→wire effort map and enabled-mode omission policies. Validation rejects incomplete/unaccepted maps at adapter construction.
- The DeepSeek profile exposes exact `none | high | max`: `none` emits `thinking.type=disabled` with no effort; high/max emit enabled plus their exact scalar value. Default high materializes only for a catalog-proven reasoning model. An unknown custom model with no explicit reasoning remains conservative and does not inherit V4 claims.
- Generic Chat tool defaults are also provider assumptions. DeepSeek thinking requests retain tool schemas but omit unproven `tool_choice` and `parallel_tool_calls`; PDS03 separately proves required reasoning-state continuation.

## 58. Required continuation state needs ingress and egress validation
- Checking only replay is too late: an adapter could accept a DeepSeek thinking tool response with no reasoning, publish it as valid provider state, and poison the next request. Checking only output is also insufficient: an older/crashed/tampered log may contain a neutral assistant fallback or malformed state. PDS03 validates both completed response state before successful finish and projected input state before transport.
- A generic `ChatMessage` can represent assistant tool calls but has no `reasoning_content`; it therefore cannot satisfy DeepSeek thinking continuation. Same-route C04 projection must supply the complete `ChatAssistantMessage` provider item. A nonempty tool-call array without nonempty string reasoning fails with safe field `provider_state` and no request bytes sent.
- The requirement is conditioned on the resolved thinking mode, not provider name or model spelling. `none`/disabled accepts non-reasoning tool history. High/max/default-high require it. This policy remains route data in `ChatThinkingConfig`, and the response parser emits no provider-state or Finish event when the requirement fails.

## 59. A composition harness must own teardown before filesystem cleanup
- A helper returning a bare `Context` while its `TempDir` stays in a separate caller variable permits the root to disappear before plugin effects stop; returning options also duplicates production composition. Q01 consumes `RealCompositionHarness` into `ComposedTestWorld`, calls the real `compose_world` factory/loader path and retains the root beside the context.
- `ComposedTestWorld::Drop` runs `Context::shutdown()` before taking/dropping its temp root; explicit `shutdown()` is idempotent. Tests no longer rely on Rust local declaration order or silently drop disposer closures without running them.
- “Real composition” means real settings/credentials/authorization/onboarding/session/prompt/tools/catalog/agent/TUI plugins and factory ordering. Only inference is the sanctioned scripted fake. The exact inventory audit uses the shared harness, so adding a default plugin changes the common proof rather than a disconnected smoke.

## 60. A fragmentation harness must split raw SSE bytes, not decoded events
- Giving every adapter the same prebuilt `SseEvent` vector cannot catch CR/LF, UTF-8, multi-data or EOF behavior; fragmentation has already disappeared. Q02 cases carry raw network chunks and `SseFixtureTransport` runs the production `SseDecoder` before the protocol adapter sees events.
- A network error after partial bytes is not clean EOF. Calling `decoder.finish()` first could publish an unterminated partial event and then the failure. Terminal-error cases suppress EOF flushing and yield the injected error immediately after any already completed events.
- Each matrix case receives a fresh transport/service and records its call count. Whole, bytewise and every two-fragment split run in deterministic order. The async closure owns adapter construction, so the same runner now proves both Chat Completions and Responses rather than baking one protocol into the harness.
- Fixture labels are bounded safe ASCII ids; source/version/capture metadata belongs to CAT08/Q08, not this byte/transport foundation.

## 61. Saying “the roadmap is here” is weaker than linking its authority
- DOC01 found that README linked the engineering directory/master plan but not the authoritative dependency tracker. A human could navigate there, but automation and future handoffs had no direct, testable authority edge.
- The freshness test covers README, STATUS, the historical completed TASKS backlog and FEATURES. Each must directly link both the master program and active TASKS using a path relative to that document, and every resolved target must be a real file. A matching label or directory link is not enough.

## 62. Broad plugin families cannot answer “who installed this row?”
- `PluginDescriptor.contributions=[Tool,Provider]` says what a plugin may do, not whether `read`, `deepseek`, or an MCP process actually exists. K04 adds exact registry namespaces and names, with the runtime plugin id on every row. Services are attributed automatically at successful `Context::provide`; static rows are declared before apply; dynamic MCP rows commit during apply.
- Exact uniqueness is per namespace, not one global string. A DeepSeek inference provider and DeepSeek model catalog may both be named `deepseek`; `InferenceProvider` and `ModelCatalog` keep them unambiguous. Duplicate kind/name claims across plugins fail composition naming both owners.
- The context marks one active applying plugin. Dynamic contribution calls outside that boundary fail; descriptor recording cross-checks every built-in exact kind against its broad declared family. Unclassified compatibility/test plugins are exempt until migrated.
- Diagnostics need a shared live handle, not a post-hoc CLI guess. `PluginInventory` records descriptors only after apply succeeds and powers `/plugins verbose`. The exact composition test compares inventory services/tools/commands/prompt sections to their live registries, so missing attribution is a gate failure.

## 63. Implementation source and activation scope are different axes
- A compiled built-in implementation may be selected by a user/session/managed layer; calling its source “user” loses provenance, while calling its scope “built-in” hides the effective decision. K05 records `PluginDescriptor.source` and `AppliedPlugin.scope` separately in the live inventory.
- Caller-provided layer order must not change precedence. The resolver rejects duplicate scopes, sorts by the fixed built-in < user < project < local-project < session < managed order, rejects duplicate ids within a layer, and validates kebab-case ids.
- Enabling an existing row updates its winning scope without moving it; newly enabled or re-enabled-after-disable rows append in directive order. That preserves dependency-sensitive base order while making overlay additions deterministic.
- Scope resolution chooses one plugin instance; it does not authorize two service implementations to shadow each other. `PluginFactories::build_scoped` consumes each winning factory once, and normal composition collision/injection/inventory checks still apply. Managed is a final constraint layer, not an escape from those invariants.

## 64. A profile file cannot authoritatively declare where it came from
- Scope/source metadata is assigned by trusted discovery, not parsed from attacker-controlled profile bytes. K06 `ProfileDocument` contains version/name/plugin decisions only; `ProfileLayer::new` binds it to a matching typed `ProfileSource` and rejects empty/unsafe metadata or scope-source mismatch.
- An enabled-only plugin list erases why a row disappeared and which layer won. `EffectiveProfileTree` keeps every precedence-ordered layer, every plugin ever mentioned, every enable/disable decision, the last source/scope, and a separate enabled composition order derived through K05.
- Standalone profiles have their own strict schema v1, independent of root config schema v4. Unknown fields, missing/newer/older versions, malformed names and duplicate plugin rows fail. K07 owns file discovery/selection and may add explicit migrations later; K06 does not silently reinterpret bytes.

## 65. CLI and picker profile selection must share the loaded layer
- Parsing the same file twice in separate CLI/UI code paths invites path, version and precedence drift. K07 `NamedProfileStore` is the only discovery/load boundary; list is sorted and validates the same strict documents that `load` returns to either consumer.
- Profile names are lowercase kebab-case file stems under the fixed `$HEYCODE_HOME/profiles` directory. Traversal, symlink/non-file roots or entries, files over 1 MiB, invalid schemas and embedded-name mismatch fail before composition. Source metadata is attached as `NamedProfile`, never trusted from bytes.
- `compose_world` accepts already-discovered `ProfileLayer`s and feeds them through K06→K05→`build_scoped`/`compose_scoped`. ACP and normal run modes pass the same layers. A legacy complete `[profile] plugins` list plus `--profile` is ambiguous and fails rather than guessing merge semantics.
- Duplicate `--profile` flags fail. `setup` rejects `--profile` because it edits base connection settings, not an overlay. Profile selection may disable optional plugins, but dependency/inventory/service collision checks remain authoritative.

## 66. Composition doctor's graph phase must inspect declarations, not activate plugins
- Running normal composition to diagnose it can resolve credentials, create sessions/files/watchers, hit keychains, start MCP children or make network-capable objects live. K08 splits `resolve_world_plugins` from apply and keeps `inspect_world` over the exact scoped vector without calling any plugin `apply` or disposer. The later isolated activation phase in #207 is a separate report, not a weakening of this API.
- Dry dependency analysis needs static provided-service declarations. `Plugin::provides` is the plan; successful `Context::provide` remains runtime truth. The old agent fallback that conditionally invented `agent-options` made those graphs disagree, so `agent-options` is now an explicit injection and fail-loud dependency.
- A blocked plugin does not make its declared services available to later rows. This produces useful cascading diagnostics while preventing a dry report from pretending failed activation provided anything. Duplicate plugin ids, descriptor drift, missing injections, service/exact collisions, invalid exact names and broad-family mismatch have stable codes.
- `inspect_world` constructs the same production factories/profile layers as `compose_world`, but returns a safe report even when world resolution fails. Graph-invalid `doctor --composition [--json]` exits 1 without activation. A healthy graph may proceed only into #207's explicit disposable probe; K09 owns its transactional activation health, and operational checks extend S11's registry only through their owning tasks.

## 67. Making an implicit service explicit is a persisted profile migration
- Removing the agent's conditional `agent-options` fallback correctly made dry and runtime graphs agree, but older custom complete profiles named `agent` without the previously invisible provider plugin. Treating those profiles as malformed would turn an internal cleanup into a startup regression.
- Root config schema v6 materializes explicit dependencies for older documents only: v2→v3 adds `agent-options` before `agent`; v3→v4 adds `ui` before `tui`; v4→v5 adds `runtimes`; v5→v6 adds `routing` plus minimal `settings`, `models`, `runtime-native` prerequisites before `tui`. They preserve every intentional row and unrelated byte decoration, report typed redacted changes, back up home config exactly and leave explicit/project files pending.
- Do not solve this with dependency auto-closure in composition. Complete profiles remain exact and fail loud; future plugin dependency changes require an explicit versioned migration or a deliberate compatibility decision.

## 68. A live doctor must not diagnose by booting the product world
- Full production composition can create a session, open/migrate stores, start watchers or MCP children and construct network-capable services. S11 `doctor` composes a restricted health world containing only the doctor registry, non-watching settings reader, non-resolving environment/keychain registrations and K08's typed zero-apply graph. The dedicated `doctor --composition` activation phase uses #207's disposable substitutions rather than the live product world.
- `DoctorCheck` registration is an effect with duplicate-id refusal and deterministic order. One cancellation token owns a run; pre/current/remaining checks become structured skipped results. Panics and invalid outcomes become fixed failure codes instead of starving later checks or leaking error text.
- Result summary and repair text enters only as compile-time-static strings, while runtime evidence is a closed enum (currently the already-redacted `CompositionReport`). Do not add arbitrary `String`, JSON or provider-error evidence fields; add a reviewed typed variant or wait for S15's redaction verifier.
- Warnings are healthy but visible. Failures and skipped checks make the report unhealthy and the CLI exits nonzero. Human and schema-v1 JSON forms are projections of the same report, and canary tests must cover every future runtime-text evidence variant.

## 69. Setup metadata and test credentials have different isolation boundaries
- A provider/model array in the binary becomes stale independently of the provider adapter and catalog. B09 projects `ProviderProfile`s from the composed `providers` service and models from `CatalogRegistry`; registry name/descriptor mismatch, blank defaults and empty services fail. Catalog-backed rows exclude retired models. Missing/failed catalogs visibly use the provider default and only then allow a custom id.
- Setup discovery needs real catalog plugins but not a dispatch-capable client. `SetupWorld` uses metadata-only providers and excludes sessions, agents, tools, TUI and MCP. Configuration serialization uses TOML values (never interpolation), atomic `0600` replacement and symlink refusal.
- `$HEYCODE_HOME` does not isolate the macOS Keychain: its service/account namespace is process-global. A manual B09 PTY probe demonstrated that a dummy key can shadow a real file fallback even under a temporary home. Never enter test credentials through the live system Keychain. Use an environment canary or injected backend; wait for the complete prompt before sending masked input so terminal echo cannot race raw mode.

## 70. Redacted migration JSON must project the notice, never the plan
- `ConfigMigrationPlan` deliberately retains original/rendered bytes privately for source-CAS apply. Serializing it, adding a debug/raw field, or reparsing unknown tables for diagnostics would turn future extension secrets into output. B10 maps only `ConfigMigrationNotice`: path, version class, applied/pending disposition and the closed semantic change enum.
- Startup used to print the notice and discard it before composition. `WorldOptions` now carries an optional borrowed notice into both product and restricted doctor worlds; `doctor-config` owns the `config-migration` check. Tests/harnesses pass `None` explicitly rather than inventing migration state.
- Current/applied are pass; user-owned pending is a warning with repair guidance. Human and schema-v1 JSON render the same typed evidence. Canary coverage must include unknown raw config fields and both stdout/stderr, not merely assert that known structs lack a field named `secret`.

## 71. A synchronous credential seam may already run inside Tokio
- Calling `Runtime::block_on` from `CredentialProvider::resolve` panics when composition/dispatch already runs inside the product runtime. S10 moves exact command execution onto a named private worker, creates one current-thread runtime there, joins synchronously and maps worker/runtime failures to fixed safe codes.
- Do not use `Command::output`: a chatty child can fill its pipe before exit and defeat timeout. S10 drains capped stdout concurrently, times the direct child, kills/reaps on expiry and bounds pipe settlement. Output is one UTF-8 line; stderr, argv, output and OS errors never enter diagnostics or `Debug`.
- Direct-child kill is not process-tree confinement. The default provider therefore has zero executable specs, and project config cannot activate commands before K12 trust plus E02/E04 process-tree/sandbox routing. Tests use direct children for timeout and never a shell that leaves descendants behind.

## 72. A UI registry should type handles without standardizing renderers too early
- Panel/dialog/status metadata is universal; rendering/action protocols are not yet. U03 stores validated descriptors plus opaque typed `Arc<T>` handles. Owning plugins define the handle contract, consumers downcast only what they support, and wrong-type lookup is `None` while poisoned registry state fails loud.
- Publish exact `ui_slot` inventory before live registry state so a collision keeps owner attribution and no partial handle. Registration then commits with a token-checked context effect; composition rollback and shutdown remove it. Same id across slots is legal, same slot/id is not.
- Deterministic order is slot, descending priority, id—not caller hash order or registration timing. The existing TUI must register real transcript/approval/session rows; a registry composed only in its own tests is not a shipped extension surface.

## 73. A command palette must project registry metadata, not reconstruct it
- Command names/help alone cannot express arguments, source, shortcuts, availability or active-turn behavior. CMD01 gives every command a validated descriptor and dynamic availability; `/help` and future U04 consume the same catalog in early+late registration order.
- Unavailable commands remain catalog rows with a trimmed control-free reason. Hiding them makes prerequisites undiscoverable. Registry lock poisoning fails `catalog/help/names/get` instead of silently dropping late plugin commands.
- Self-declared source metadata can lie unless checked. Real composition joins each descriptor id to K04's exact `command` inventory and requires the owning plugin to match. Timings are descriptive until U11; do not claim queued/interrupting behavior merely because CMD01 labels it.

## 74. A command palette is a modal projection, not another command registry
- U04 holds no command table. It refreshes `CommandRegistry::catalog` on open/refilter, and pure fuzzy ranking returns complete catalog rows. Stable tie-breaking uses registry order; unavailable rows remain searchable and Enter refuses them without mutating input.
- Ctrl+P preserves composer text; empty `/` does not first insert a slash. Selection inserts only `/id` plus a trailing space when arguments exist—not placeholder syntax and not immediate execution. This keeps argument editing explicit and timing enforcement in U11.
- Modal precedence is security-sensitive: masked secret, onboarding and approval dialogs close/preempt the palette, and Esc closes the palette before it can interrupt a turn. A palette visually covering an approval while keys route to the hidden dialog is a serious UI contract failure.

## 75. Status must describe the composed world, not echo requested config
- U05 reads runtime id/current route from `Agent`, permission from the actual `ApprovalPolicy`, workspace from the canonical composed cwd and health from `DoctorRegistry`. The renderer never maps config strings to claimed effective state.
- Doctor runs asynchronously so first paint is not blocked. Its cancellation token has a drop guard; leaving the TUI cannot detach a future network/process check. Missing/error health is explicit unavailable rather than optimistic green.
- The welcome card yields as soon as transcript content exists, but effective route/permission/health stay in the compact status line. Runtime `native` comes from `Agent::runtime_id` so R01 can replace the source without editing presentation code.

## 76. A live model picker owns a cancellable wait, not the shared refresh
- CAT02 refreshes are shared/single-flight. U07 cancellation or a replacement Ctrl+R aborts only the picker waiter; it must not cancel a refresh another consumer joined. A parent drop guard settles picker waits on TUI exit.
- Filters use normalized evidence: selectable excludes effective retirement, and Tools/Reasoning require explicit Supported. Unknown is not false in badges, but it is also not sufficient for a support filter. Enter selects only a currently filtered catalog row—never free text or a retired row.
- Stale fallback is usable only when visibly labeled with its safe warning; live, fresh cache, stale and error are distinct. `/model` updates live Agent state in U07; CMD02 owns persistence and C14 now gates opaque-state route transitions explicitly.

## 77. Command scheduling follows task settlement, not spinner presentation
- `verb` may arrive late, change for presentation or clear before a task's `JoinHandle` settles. U11 tracks active-turn lifecycle separately and promotes its FIFO only after both turn and command task handles are absent. A model-scheduling command runs in its own task so its nested `Agent::send` cannot freeze terminal/UI event processing.
- Queue exact command text privately, but narrate only the validated descriptor synopsis. Echoing arguments can disclose title text, prompts or future credential-bearing input. Availability and timing are rechecked at dispatch because a queued command's prerequisites may change while it waits.
- Interrupting commands are cancel-default: show a modal, restore the exact composer text on cancel, invoke the single current interrupt handle only after explicit confirmation, then wait for settlement before execution. This does not solve reusable cancellation; the native agent's process-lifetime token is replaced by A04 before later turns may safely follow an abort.

## 78. Creating a temporary home does nothing unless the child receives it
- A U11 verification created a `mktemp` directory in one tool call, then launched the doctor in another without prefixing `HEYCODE_HOME`. The real `/Users/naresh/.heycode/config.toml` therefore followed normal startup migration from schema 1 to 4 and gained `.v1.bak`. Secret-free comparison proved the current and backup payloads differ only at `schema_version`; both are regular files, and no provider request or credential resolution ran.
- Resolve the absolute temporary path first, then put `HEYCODE_HOME=/absolute/path` in every independently launched command. Never assume a shell variable or prior tool-call environment crosses process boundaries. Verify the reported path/output, not merely the exit code.
- Do not silently roll back an automatic migration. Preserve its backup, inspect semantic equivalence without printing config contents, disclose the mutation, and ask before restoring when restoration would overwrite valid current state.

## 79. `/init` owns a managed section, not the user's instruction document
- Generating a plausible AGENTS.md is easy; safely improving an existing constitution is the real contract. CMD05 recognizes exactly one versioned start/end marker pair, preserves every byte outside it, and refuses missing/repeated/reversed markers rather than guessing ownership. New guidance reports only fixed manifest names and commands—never the absolute workspace path or existing user text.
- Preview is a no-write operation. Its opaque token hashes both the observed AGENTS.md state (including absence) and the generated proposal, whose contents depend on detected manifests. Apply re-derives the proposal and repeats the check immediately before atomic commit; any earlier edit or workspace-fact change becomes `StalePreview`. Existing file mode is preserved and a new file is `0644`.
- Bound reads before allocation, require UTF-8, refuse symlink/non-regular targets and cap rendered diffs. Atomic rename prevents torn documents, but ordinary filesystems do not offer a compare-and-swap against an unrelated editor in the final check→rename interval; do not claim cross-process linearizability.

## 80. A plugin command must disappear with its owning effect
- `CommandRegistry::register_shared` originally made late rows live for the registry's entire lifetime. That was acceptable for hand-built tests but violated plugin rollback/shutdown semantics for skills, plan and future commands.
- `register_effect(ctx, command)` stores an opaque registration token and registers a disposer that removes only the matching row. Token matching prevents an old disposer from deleting a later same-id owner. CMD05 uses this path, and skills/plan migrated with it; keep `register_shared` for explicit registry-lifetime callers only.
- Static exact inventory still publishes before apply. A failed duplicate registration aborts composition and the context rollback unwinds already-installed command effects; live catalog/help output therefore cannot outlive the plugin that owns it.

## 81. A coding-agent runtime is not an inference provider with more methods
- Inference adapters let the native heycode loop own prompt/tool/session semantics. Codex, Claude and OpenCode subscription integrations own their loop and session lifecycle, so implementing `InferenceAdapter` for them would blur credentials, double-run tools and lose provider-native permissions/compaction.
- R01 gives `AgentRuntime` its own exact `agent_runtime` inventory namespace, `native|delegated` kind and `runtimes` registry. Provider/model pickers can now distinguish loop ownership without guessing from names. A plugin may eventually contribute both contracts, but each registration and capability claim remains independent.
- The base registry deliberately starts empty. Do not register a fake `native` row merely to satisfy UI; A01 must adapt the real `Agent` and its durable session behavior before publication, while R03/R07/R10 own delegated implementations.

## 82. Runtime lifecycle methods need caller cancellation and quiescent close
- Account/model discovery, start/resume/fork and every session operation accept one explicit caller `CancellationToken`. Session `cancel` interrupts active work but keeps the session reusable; `close` is idempotent and resolves only after owned streams/tasks/processes settle. Mixing these meanings recreates the poisoned-turn and orphan-process classes.
- External session/turn/request ids are validated opaque newtypes. Errors expose stable `unavailable|unauthorized|unsupported|not_found|conflict|cancelled|protocol|invalid_request|closed|internal` classes and bounded control-free safe text—never raw CLI/app-server bodies or subscription tokens.
- Registry entries capture one immutable descriptor and an opaque registration token. Sorted discovery is deterministic; duplicate ids fail before publication; rollback/shutdown removes only the token-matching implementation. R02 still owns validation of event sequence, text bounds and terminal settlement before durable/UI projection.

## 83. The native runtime stream should bridge committed session events, not duplicate UI deltas
- `UiEvent` omits durable call ids and can arrive before/around persistence. A01 listens to the Session's post-commit bus for turn/chunk/final/tool/usage/settlement events, using UiEvent only for permission requests and safe notices. Listening to both planes for assistant/tool data would duplicate output and invent call correlation.
- The bridge owns a 1024-event replay ring plus bounded broadcast. Subscribe snapshots history while holding the same state lock used by emit, then receives later events—no snapshot/live gap or duplicate. Slow subscribers get a classified protocol error rather than unbounded memory growth.
- Event listeners themselves are registrations. `EventBus::on_effect` stores the exact erased listener cell and removes it on context rollback/shutdown. Native registry disposal, UI listener removal and session listener removal unwind LIFO; held Agent/bus handles cannot keep receiving through a dead plugin.

## 84. A session-scoped native runtime must not pretend to be a session factory
- One composed world owns one durable Session and Agent. `runtime-native` therefore accepts start/resume only when heycode session id, absolute workspace and provider-native session id match that world. Starting a non-empty log is a conflict; it must be resumed. Fork remains Unsupported until a composition/session factory owns it.
- Native send serializes through one gate, increments active lifecycle state, and always waits for Agent cancellation to durably settle before returning Cancelled. Compact shares the gate/active counter. Close flips closed, requests cancellation, then waits for zero active work; context shutdown cancels held sessions before disposing listeners/registration.
- A04 replaced the former process-lifetime turn token. The Agent plugin owns terminal shutdown; each send owns a fresh child lease, so a cancelled turn no longer poisons later sends.

## 85. Cancellation of the active turn and shutdown of the Agent are different operations
- A single Agent-lifetime token made Esc permanent: once cancelled, every later provider select immediately aborted. A04 separates one terminal shutdown token from an optional active child token identified by an opaque Arc generation. Lease drop clears only itself, preventing an old completion from erasing a newer turn.
- `AgentCancellation::cancel` clones/cancels only the current child and does nothing while idle. `shutdown` is the only permanent operation and is registered once by the Agent plugin as a context effect. Poisoned cancellation state fails closed; the async turn gate prevents overlapping native sends from publishing two active leases.
- A caller token is independent input. `send_cancellable` races it while waiting for the gate—so pre-admission cancellation writes no user/turn event—and during provider streaming, where it follows the same durable aborted-settlement path. Never drop an admitted turn future merely because the caller disappeared.

## 86. Install the operation token before spawning work, and keep UI interrupt handles reusable
- Registering the Agent's child lease inside an async future leaves a small spawn→first-poll window where `cancel()` sees idle and is lost. `runtime-native` stores a child of the caller token before invoking Agent; the TUI stores its caller token before `tokio::spawn`. Cancel can therefore land at every scheduling point.
- Native session cancel targets the stored operation child, which also covers waiting on the Agent gate; close cancels the same child then waits for the active counter. Direct Agent cancellation still works through the reusable handle. After settlement, identity-checked guards clear both layers.
- TUI `interrupt_fn` was `FnOnce` and consumed by the first Esc/interrupting command. It is now a reusable `Fn + Send + Sync`; Esc, Ctrl+C and interrupt confirmation borrow it rather than `take()` it. Approval/modals retain their precedence, so one key still performs one action.

## 87. Provider ids and agent-runtime ids occupy different route planes
- An inference provider supplies model calls inside the native loop; an AgentRuntime owns the loop. A combined picker that shows one untyped name list makes Codex subscription look like an OpenAI API endpoint and encourages credential/behavior bugs.
- U08 projects complete live registry descriptors into explicit Inference API, Native agent and Delegated agent classes. Identity is class+registry id, so identical text cannot collide. Search includes id/display/detail/class; filters operate on the class enum, not name heuristics.
- Discovery is not activation evidence. The current native runtime and registered inference APIs are live-selectable; delegated/non-current native rows stay visible with a prerequisite until a primary-session bridge is actually wired. Never update welcome/status or settings for an unavailable row.

## 88. A UI handle may hide a new service dependency from plugin apply
- `TuiPlugin::apply` only registers renderer handles, while `TuiHandle::run` consumes LoopDeps later. Making `runtimes` optional in the binary would let composition pass and fail only when `/provider` opens—violating fail-loud loading.
- U08 adds `SERVICE_RUNTIMES` to TUI injects and makes LoopDeps hold the concrete registry. Config schema v5 inserts `runtimes` immediately before `tui` in older custom complete profiles; built-in profiles pick it up automatically. Migration tests accumulate earlier `agent-options` and `ui` prerequisites too.
- Treat delayed handle/run dependencies exactly like apply-time reads: declare them statically, update dry inspection, migrate persisted exact profiles and test both the old-version edge and real composition.

## 89. Persist a route before publishing it live
- Updating `Agent::set_provider/model` and then writing settings announces state that may roll back. CMD02's `RoutingService` validates the candidate, reads the immutable routing snapshot revision, performs `replace_user(..., expected_revision)`, parses the committed effective snapshot, and only then updates Agent.
- The plugin-owned schema captures the composed provider ids and the one currently activatable native runtime. Unknown routes, partial explicit tuples, non-null unsupported effort and unknown fields fail before publication. Provider default models may bootstrap without a catalog; every non-default model requires current selectable catalog evidence.
- A settings watcher applies valid external generations through the same effective-route function. Settings persistence/revision/CAS tests own durability; routing tests prove the live Agent and restart composition observe the persisted provider/model.

## 90. Optional auth commands must not turn routing into a hidden full-stack dependency
- The first CMD02 implementation made one `routing` plugin inject settings, models, credentials, authorization and onboarding. Schema migration then broke an intentional minimal TUI profile by silently requiring services it had never selected.
- Split by capability: `routing` owns selection/service and provider/model/effort commands; `routing-auth` owns connect/logout and alone injects the auth stack. Built-in profiles include both. Schema v6 adds only settings/models/runtime-native/routing to legacy TUI profiles, preserving their previous auth surface exactly.
- Test the historical minimal real binary after every dependency migration. Unit migration order can be internally consistent while still producing an unsatisfied plugin graph.

## 91. Do not persist an option that no active request consumes
- Reasoning effort vocabularies are adapter/model specific. The current legacy native dispatch does not expose exact accepted effort ids or consume a routing effort field; accepting `/effort high` would create durable but ineffective state.
- CMD02 still contributes `/effort` with full descriptor/source/help metadata, but dynamic availability explains that the active adapter exposes no levels and dispatch refuses execution. When P04/P10 activates an adapter-owned vocabulary, the same routing namespace/command can safely admit only published ids.
- “Unavailable with reason” is implemented command behavior, not an omission. UI keeps the row discoverable and prevents it from mutating the composer/runtime.

## 92. Process cancellation is complete only after the contained tree settles
- Killing a direct `Child` is not a process-tree contract. E02 uses one processkit private group per exact launch, rejects a handle that does not advertise kill-on-drop, and proves explicit kill and handle drop against a real descendant survival marker. The reported containment mechanism remains truthful: the POSIX process-group backend kills ordinary descendants but is not resistant to a deliberate `setsid` escape.
- Environment safety is construction, not a denylist. `ProcessSpec` carries the complete explicit environment and the local provider always clears inheritance before applying it. Specs/results/errors use body-free `Debug` or fixed messages so argv, values, captured output, paths and raw OS errors cannot leak through ordinary diagnostics.
- Caller cancellation is forwarded into the one operation token, not implemented by dropping an in-flight future. A spawned handle owns the forwarding task; wait/cancel/terminate/kill abort and join it after terminal settlement, while synchronous drop aborts it and relies on the private group's kill-on-drop. Context shutdown cancels the parent token. This seam alone does not route existing bash/MCP/credential children; that claim waits for E03/E04 consumer migration.

## 93. Resolve shell defaults once, then preserve the resolved spec through policy
- Optional caller intent and executable facts are different types. `ShellRequest` holds command plus optional cwd/timeout/output overrides; only the provider's `resolve` materializes platform shell argv, absolute cwd, explicit environment, deadline and capture policy into `ShellSpec`. `execute` accepts no request and performs no defaulting. A malformed `timeout_ms` must fail at the model boundary—not disappear into the configured default.
- Confinement transforms argv after resolution but must preserve every non-argv fact. E03 keeps the existing sandbox wrapper as `ShellSpec::with_launch_argv`, which revalidates the absolute wrapper program and copies cwd/environment/timeout/output policy exactly. E04 moves that transform below all process Consumers; until then do not claim MCP or credential commands use it.
- Cancellation must reach tools, not only provider streams. `ToolCtx` now owns an operation token; Agent races both turn and caller cancellation, cancels that token and awaits the tool future. A live regression launches a background shell descendant, cancels the turn, and proves abort returns only after the survival marker is prevented. Adding `shell` as a delayed `tools` dependency also requires schema-v7 insertion of both shell and subprocess rows; ACP's relative cwd had to become absolute at its protocol boundary rather than weakening `ProcessSpec`.

## 94. An optional sandbox backend still requires a mandatory policy path
- Absence of a `sandbox` service lets Consumers bypass policy by checking `Option`. E04 always publishes `SandboxService`: `Off` has no backend but remains the one truthful path; read-only/workspace require exactly one backend. `subprocess-local` injects the service and applies its transform after every spec is resolved but immediately before all capture/spawn/interactive launches. Bash no longer sees a sandbox handle at all.
- Interactive protocols must not punch a tokio-specific hole through the seam. `InteractiveProcess` splits an owned process controller, input writer and line reader behind provider-neutral traits. MCP resolves its executable and explicit environment through the service, and command credentials keep their synchronous worker only as an async bridge. Descendant-marker tests prove MCP shutdown and credential timeout kill whole trees; a source scan rejects new production spawn APIs outside the local backend, with Landlock's same-process `exec` as the named exception.
- Runtime ownership matters during shutdown. Waiting on a streamed MCP process while its driver still owned stdin/lines deadlocked; dropping the private-group handle after cancelling and joining the driver is the correct current teardown. Tokio also forbids dropping a runtime inside async context, so each MCP driver thread is owned/joined and its final runtime Arc is dropped on a plain thread. Schema v8 makes sandbox mandatory before subprocess and restores missing MCP dependencies without reintroducing per-Consumer checks.

## 95. “Backend available” and “sandbox effective” are different facts
- Full Access must not disappear just because a backend is installed, and an installed backend must not imply confinement is active. The E10 report carries both `active_backend` and `available_backend`; effective Off keeps the candidate for Read Only/Workspace choices but `subprocess` returns the original spec without calling it. A live recording test pins zero transforms in this state.
- Choice visibility follows evidence, not platform names. Backends publish tri-state support for read-only, workspace-write and network isolation; an active restrictive mode requires Supported at service construction. Missing/Unknown/Unsupported stays visible but unselectable. Landlock preflights its live kernel ABI before becoming the Linux candidate; later E11–E13 runtime CI remains the stronger proof.
- Filesystem confinement is not network confinement. Current Seatbelt profiles allow by default except writes, Landlock rules handle filesystem rights only, and bwrap argv omits network unshare. Every restrictive choice therefore reports host network available. UIs/commands must render that explicit fact and never collapse the report into a green “sandboxed” badge.
- “Temp writable” is not “temp isolated.” Seatbelt and Landlock grant host `/tmp`/`/private/tmp`; bubblewrap mounts a private `/tmp`. The shared Workspace Write row therefore says backend temp roots with isolation not guaranteed. Add a distinct evidenced fact before promising private temp storage.
- “Host reads” is deliberately broad, not byte-identical. Bubblewrap overlays `/dev` and `/proc` even though its root is read-only-bound from the host. UI copy calls out backend virtual mounts; add a richer read-scope fact before promising every host path/namespace is unchanged.

## 96. A filesystem seam owns behavior, not just `std::fs` calls
- E01 publishes `filesystem-local` and makes `tools` inject the `filesystem` service. Path resolution, observations, metadata, reads, writes, edits, glob and grep all cross the replaceable Provider; a recording Provider proves the five model file tools cannot bypass it. Schema v9 inserts the Provider before `tools` in older custom profiles.
- Provider outputs need validation just like network output. Search results are count/size bounded, strictly ordered and normalized relative paths; diagnostics and Debug omit paths, bodies and patterns. Wildcard matching is iterative—recursive `*`/`**` matching turned a valid 64 KiB boundary into a stack/exponential denial of service.
- Validate mutation metadata before telling the model the outcome. A multiline edit originally committed, then its own service rejected newline-bearing diff metadata and returned an error. The output contract now admits bounded multiline previews, prefixes every diff line, rejects zero-match `replace_all`, and tests line-boundary replacements. Canonical roots, symlink swaps and read/mark TOCTOU remain E05 and must not be claimed by E01.

## 97. Platform sandbox claims require native probes, not cfg-success
- The first Linux runtime lane found incorrect hard-coded Landlock UAPI bits and an unconditional `/private/tmp` grant that failed on Ubuntu. Keep ABI-v1 rights exact, add newer rights only when the running ABI supports them, require restriction status, and omit nonexistent grants deterministically. A cross-compile proves syntax; only native kernel execution proves enforcement.
- Native matrices distinguish launcher failure from a denied child by unique readiness/exit markers. They cover host reads, read-only/workspace writes, outside and symlink escape, descendant inheritance, temp/device paths, TCP and pathname Unix sockets. Current Linux/macOS policies are filesystem-only, so successful networking is expected evidence, not a failed test.
- Windows Job Objects prove tree cleanup, not filesystem confinement. With no Windows sandbox Provider, Full Access stays selectable while Read Only/Workspace Write stay visible, unsupported and fail closed. E13 remains active until native CI runs and a spawn-time AppContainer/restricted-token-capable seam exists; never relabel process containment as a sandbox.

## 98. Permission selection is intent until one owner commits policy
- U09 and CMD03 consume the same live `SandboxCapabilityReport`; neither reads requested config or reconstructs backend guarantees. Full Access, Read Only and Workspace Write remain visible, but only evidenced rows are selectable, and host networking is spelled out as “not isolated.”
- The current process policy cannot hot-swap safely. Selecting a different supported row yields only the exact restart prerequisite (`--set sandbox.mode=...`) and does not mutate status. Selecting the already-effective row is a clean no-op. A future Settings owner must commit a generation before UI/status changes.
- `/status`, `/doctor`, `/permissions`, `/auto` and `/sandbox` are effect-owned human-plane commands from plugin `status`. They report the live Agent, DoctorRegistry and SandboxService, reject invalid arguments without echoing them, and never become model/session messages. `/permissions ask|auto|deny` switches the session's `SwitchableApproval`; `/auto` is the no-argument alias for auto approval. Both report that the change is session-only rather than implying a persisted config mutation.

## 99. Delegated runtime events need one strict normalization boundary
- R02 accepts raw provider events and returns `NormalizedRuntimeEvent` only after contiguous sequence, session/turn/tool/request correlation, payload bounds, terminal-control rejection and settlement checks. Live EOF is a protocol failure; finite replay EOF is allowed only between settled turns. The first violation poisons the normalizer.
- Usage belongs to inner model steps and may repeat within one delegated runtime turn; it is not the inference stream's single terminal Usage invariant. FinalMessage and TurnFinished, not Usage, constrain terminal ordering.
- Normalized Debug contains only sequence and a stable phase tag. Errors never echo provider payloads, and nested JSON string values and object keys are checked for unsafe terminal controls before durable/UI projection.

## 100. Durable inbox identities are insert-once across the whole session
- C06 adds v2-only `agent/inbox/splice` for typed follow-up, steer and inject queues. Append validates shape/bounds/target and the complete historical identity ledger before writing; publish happens only after the durable append commits. Resume rejects malformed or reused ids.
- Claim, cancel and replacement settle an occurrence but never make its id reusable. This must hold across compaction and restart, not only in the current pending maps. Compaction shadows model history, never pending operational inbox state.
- Inbox text remains outside provider projections until admission is durably represented as `user/message`. Adding the kind required all five session touchpoints and an explicit v1 rejection; an ignore arm is not permission to leak it into model input.

## 101. Anthropic history is an ordered block protocol, not Chat roles with renamed fields
- P05 round-trips complete assistant content arrays: thinking/signatures, redacted thinking, citations, client/server tools, compaction, unknown complete blocks and continuation containers. Same-route state replaces the generic assistant copy. `pause_turn` is a distinct `FinishReason::Pause`; N02 owns automatic continuation.
- Parallel client results must be one immediate user message containing every `tool_result`. Consecutive neutral Tool messages are coalesced in order, and the durable session `is_error` fact crosses `WireMessage`/`ChatMessage` so failed results emit `is_error:true` without guessing from text.
- Anthropic total prompt usage is `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`; components are cumulative/monotonic and addition is checked. Response model identity, content/message lifecycle, thinking-signature order, exactly-one compaction delta and both null start-terminal fields fail closed. Provider-controlled discriminator/error strings never enter diagnostics.

## 102. An MCP snapshot is not the private transport definition
- Exact stdio arguments/environment may contain sensitive literals, so `McpServerDefinition` exposes trusted getters but no Debug/serde. Schema-v1 snapshots show value source kinds and credential references while omitting literal bytes; Streamable HTTP headers are reference-only and URL userinfo/query/fragment/invalid authority forms fail.
- Definition and connection registrations are context effects with opaque tokens. Successful generations advance only at one atomic commit; failed candidates do not advance, immutable snapshots retain their old generation, and disposed/replaced publishers cannot clobber a later owner. Registry shutdown is terminal even for held service handles.
- State must agree with the immutable definition: a successful credential-backed generation proves Connected, no-credential definitions cannot claim auth failure/required, reconnect attempts use the enabled configured budget, and last-good retention is explicit. The existing stdio bridge moves beneath this registry in MCP02; MCP01 does not fake persistence or mutation without a Consumer.
- Once MCP02 makes the stdio bridge inject `mcp`, historical exact profiles need the dependency too. Schema v10 inserts `mcp-registry` immediately before an existing `mcp`; relying on the built-in order would make custom profiles fail at composition after upgrade.

## 103. Plugin manifest portability is a security boundary
- PL01 parses a closed schema-v1 TOML document into validated metadata only; it does not install or activate code. Plugin/contribution ids are namespaced, API/platform compatibility is explicit, collisions and registry-authorized overrides are atomic, and authentication can represent credential references but never values.
- “Relative” must be portable. Reject drive prefixes/colons, backslashes, traversal, empty components, Windows device names (including case-insensitive COM/LPT ASCII and superscript-digit forms), trailing dots, and case-insensitive contribution-path collisions before an installer joins paths. PL02 still owns symlink-safe extraction and content-addressed installation.
- Ed25519 metadata means one canonical base64 encoding of exactly 64 bytes—not merely any base64-looking string. Source authorities reject credentials, percent-obfuscated userinfo and invalid ports; dependency SemVer bounds compare precedence without confusing build metadata with equality. Errors use stable field paths and never echo rejected document bytes.

## 104. Workspace trust is an authority handoff, not a welcome-card preference
- Open the trust service from a canonical cwd and an absolute owner-controlled home before automatic project config/profile/settings/skills/MCP discovery. Relative `HEYCODE_HOME` or a trust service whose identity differs from `WorldOptions.cwd` fails; never anchor user authority to the repository as a fallback.
- Unknown interactive worlds may compose only enough safe capability to render the highest-priority modal. Model sends, slash commands, paste and queued work stay blocked. A typed action mutates the live CAS-bound service, returns `RecomposeWorkspaceTrust`, tears down the pre-trust world and reloads authoritative inputs. Its temporary session root is ephemeral so no phantom session publishes before the decision.
- Path authorization must become a held capability plus an untrusted relative path. Canonicalize/check/read on ambient `PathBuf`s still permits ancestor/symlink swaps. Restricted skill roots use component-wise no-follow opens and immutable in-memory bodies. Unix durable trust uses owner/mode/nlink checks plus one cross-process lock around read/CAS/sync/rename; Windows persistence remains fail-closed until protected handle-relative directory creation exists.

## 105. Canonical containment must survive every mutation race, not only initial resolution
- E05 derives file authority from the same canonical sandbox workspace and holds `cap-std` directory capabilities. Explicit roots are bounded and read-only/read-write typed; `..`, canonical aliases, outside roots, symlink loops/escapes and crafted resolved handles fail.
- Read freshness includes stable identity plus length/timestamps; write now requires a fresh observation too. Mutation rechecks root, parent, target or target absence, writes a synced root-local temporary, preserves mode and commits with no-clobber creation or capability-relative replacement. A successful-looking path check followed by ambient rename is not confinement.
- Provider errors state the actionable re-read/root/approval recovery without echoing host paths or content. Process sandbox launch rechecks the same workspace identity; a filesystem policy and a process policy that canonicalize independently can otherwise authorize different objects.

## 106. Byte-framed protocols cannot ride a line decoder or cumulative capture buffer
- `ProcessLines` is intentionally text-oriented and may decode lossily; processkit's ordinary output policy is cumulative across a process. A long-lived JSONL protocol can therefore replace invalid UTF-8 or silently stop delivering later valid frames after crossing its capture cap.
- `spawn_interactive_raw` taps stdout before decoding, returns non-empty chunks up to 64 KiB with explicit EOF, and uses eight-chunk backpressure instead of cumulative retention/drop. Protocol owners perform their own per-frame UTF-8/size checks.
- Cancellation or teardown closes the raw receiver before waiting on the process tree. Otherwise a child blocked on a full tee can deadlock cancel/kill/plugin shutdown. Per-read cancellation consumes no bytes and does not poison the process.

## 107. Retry safety is a property of the resolved call and the bytes already emitted
- Stable provider failures retain only class/origin/status/bounded code and semantic retry advice; reqwest text, URLs, query strings and response bodies stay out of Display/Debug. Non-success bodies have both a total-byte cap and an independent deadline. HTTP 401/403/429/5xx authority cannot be contradicted by a body discriminator; body codes refine compatible 4xx only.
- Retry policy is resolved into `ResolvedCall`: bounded attempts/backoff/jitter/Retry-After, provider veto and explicit replay safety. Stateful container/native-server calls set `Never`; no normalized event may be followed by replay. `Finish` is terminal before cancellation or another inner poll, and zero/past Retry-After is a valid immediate retry.
- A tested adapter is not shipped until the Agent advertises it through `Provider::inference_adapter`, refreshes exact catalog evidence, commits request header/context, independently projects/verifies the call and dispatches with the turn token. DeepSeek uses this path; OpenRouter stays on compatibility dispatch until POR01/POR02 supply exact per-model catalog evidence. Schema v11 adds `models` before `agent` in historical exact profiles.

## 108. A delegated CLI handshake must bind the launcher, interpreter and terminal state
- An absolute JavaScript launcher with `#!/usr/bin/env node` is not an absolute execution chain. R03 sanitizes PATH to canonical absolute non-workspace directories, drops absent entries, resolves a native interpreter explicitly and launches interpreter+script. Script/interpreter digests are rechecked between pinned version and app-server spawn; this is replacement detection under the installed-binary boundary, not publisher authentication.
- Codex app-server JSONL uses the raw seam, strict UTF-8, 1 MiB frames, depth/node validation and an 8 MiB aggregate retained budget. Request admission and fail/close share one control lock; event waits use the sticky lifecycle token. Cancellation during spawn is connected to and settles that spawn.
- Close promises only the reported containment mechanism. POSIX groups settle ordinary descendants but do not resist deliberate `setsid`; expose the capability instead of saying “complete tree.” The safe live gate invokes only pinned version plus initialize/initialized with a temporary `CODEX_HOME`.

## 109. A compatibility probe must fail closed on executable or event drift
- Claude version, credential-blind auth status and the tool-free no-persistence query must use one canonical executable identity, revalidated before every spawn. Independently resolving a mutable shim after version verification lets an unverified CLI inherit the compatibility claim.
- The handshake accepts only the expected init, text-only assistant canary and exact successful result. Ignoring unknown top-level/content discriminators makes future command/tool events look tool-free. Both `--no-session-persistence` and `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1` are backstops, not permission to inspect credential files.
- R07 proves only process/account/handshake health. General sessions, callbacks, models, resume/fork and normalized events remain R08/R09 and stay `Unsupported` until implemented.

## 110. Shared-prefix lineage hashes persisted bytes, not reserialized events
- `session/created` is constructor-owned v2 metadata for safe cwd/runtime/source; legacy sessions report unknown. A fork writes only creation/lineage plus its suffix, while replay verifies and stitches the parent's exact prefix. Hash leaves are the original JSON lines under `raw-leaves-v1`; serde reserialization would make descendants depend on future formatting/migrations.
- Fork boundaries must end outside an open turn. `EventCount(0)` represents a true empty prefix, while active subagents inherit the stable pre-turn event count and append their prompt once. Parent growth after the boundary is excluded; missing/tampered/cyclic/over-depth parents fail.
- `session-query` is a replaceable, effect-owned service with bounded deterministic keyset filters/latest/resume/fork. Parent deletion must be prevented while descendants exist or the child materialized first; export/archive owners cannot treat suffix-only JSONL as standalone.

## 111. Dynamic MCP tools need owned registrations, not registry-lifetime rows
- MCP definitions/connections publish through the registry, then candidate tool schemas register atomically with token-owned RAII handles. A partial collision drops earlier candidates; Ready commits only after the complete set exists.
- Shutdown kills the transport before dropping handles. A separately held `ToolRegistry` must immediately lose every MCP row, and an old handle must not remove a same-name replacement. Registry lifetime and plugin lifetime are different ownership domains.
- Schema v10 inserted `mcp-registry` before legacy `mcp`; MCP02 now declares/injects it. Streamable HTTP/OAuth/resources/prompts remain later tasks and are not implied by stdio tool parity.

## 112. Content addressing does not make publication immutable by itself
- PL02 validates source twice through held descriptor-relative traversal, normalizes owner-only staged bytes, then hashes the canonical tree. A separate id/version reference must commit with a true no-clobber primitive; exists-then-rename can replace a winner under lock loss/race.
- Persistent OS locks plus same-process ownership and per-stage leases protect active install/cleanup. Cleanup treats a concurrently disappeared stale lock as benign, probes locks nonblocking and deletes only through the cache capability. Content objects and exact SemVer references use case-insensitive-safe keys while retaining original identity inside the verified record.
- Unix supplies the audited owner/mode/flock backend. Non-Unix install fails `UnsupportedSecurity` until equivalent DACL/lock semantics exist. PL05 now supplies marketplace provenance and pinned-digest verification — a second authority the cache does not own. Remote **fetch** and **publisher signature verification** remain unowned: the internal tree hash is not a publisher signature, and PL05 records a signature without verifying it (`SignatureState` has no `Valid` variant, deliberately).

## 113. Credential presence must inspect the configured reference
- First-run authorization correctly wrote the selected flow's `CredentialQuery`, but startup presence detection still probed a hard-coded provider-default environment name. A valid custom `llm.api_key_env` could therefore commit and read back successfully, recompose, then be classified as missing and reopen onboarding.
- Resolve the effective non-secret reference once: explicit configuration first, provider-owned default only when absent. Presence, validation, authorization-flow construction and LLM composition must all use that same reference. A production-loader test disables the system Keychain, commits through the real file provider, tears down and recomposes from the shared isolated root.
- Provider identity and credential reference are separate facts. Never infer that a selected provider must use its conventional environment-variable name after configuration or a connector supplied a different reference.

## 114. Cleanup after a loop does not cover `?` unless the loop owns the error boundary
- A `settle_authorization(...).await` placed after the TUI event loop handled ordinary `break` exits, but `?` inside the surrounding async function returned before that line. Even when current fallible branches happened before spawn or after join, the structure did not enforce the lifecycle contract and future edits could detach a credential commit.
- Run the fallible loop inside a nested async result boundary, then perform cleanup on that result. On every exit, cancel all operation tokens first, abort only non-model command tasks, and join authorization, admitted turns, commands, doctor checks and model waiters before returning the original result.
- Dropping a Tokio `JoinHandle` detaches it. A cancellation drop guard without awaiting the handle is not quiescent teardown; tests must observe the task's own settlement, including the error path.

## 115. Moving a contribution between plugins is a persisted-profile migration
- POR01 moved `openrouter-api-key` from the shared `authorization-api-key` plugin to provider-owned `provider-openrouter`. Changing only the built-in profile would make every older exact custom profile keep the old plugin id while silently losing OpenRouter connect behavior.
- Ownership migrations need their own semantic config change. Schema v12 inserts the new provider plugin immediately after the historical owner only when that owner is selected, preserves every other exact row, reports the flow/from/to ids through redacted doctor evidence and remains idempotent when the new row already exists.
- Exact contribution inventory is the proof: the default world must contain one `authorization_flow:openrouter-api-key` row owned by `provider-openrouter`, while `authorization-api-key` retains only DeepSeek. A duplicate/no-op fallback would hide ownership drift and is not acceptable.

## 116. Public catalog fields need field-specific normalization, not one string rule
- OpenRouter's list endpoint is plural (`/models`), while exact lookup is singular (`/model/{author}/{slug}`); using plural for the single row returned 404. POR02 validates the complete list before making the detail request, so an empty/partial/count-mismatched generation fails without a second network call.
- Request-facing ids and canonical slugs stay trimmed/control-free. Provider display metadata is different: the live catalog contains trailing spaces in a few names and multiline descriptions. Names are safely trimmed before publication; discarded descriptions allow only ordinary newline/carriage-return/tab controls and remain size bounded.
- Three live router rows advertise an empty `supported_parameters` array. Empty is valid explicit evidence of no advertised parameters, while duplicates/unsafe values still reject the generation. Treating every empty list as corruption made an otherwise valid 417-row generation unusable.
- Full and single-model selected fields must agree before publication. The GLM row uses the conservative current top-provider limits even though its model-level maximum is larger; routing/provider-specific semantics remain later tasks rather than being guessed from the largest number.

## 117. Provider-visible gateway policy must be durable before it reaches the wire
- Adding OpenRouter's `provider` object only inside Chat serialization would make routing choices invisible to `request/header` and C05. POR03 adds generic `ProviderRequestOption` in core, threads it through draft/resolution/snapshot/projection verification and lets the selected Provider supply it before adapter resolution.
- The generic option is provider/kind/schema tagged, object-only, recursively bounded and redacts data from Debug. A provider-specific typed policy constructs it; arbitrary config JSON is not passed through. Other providers/protocols reject unrecognized nonempty options before transport.
- Chat's option dialect is data-driven (`routing` → top-level `provider`) rather than an `if openrouter` branch. Configuring a dialect does not require every call to carry the option: direct adapter/conformance calls and pre-cancellation must still work with no option. Unsupported nonempty kinds remain fail-loud.
- OpenRouter's documented defaults are explicit policy state: fallbacks true, parameter enforcement false, data collection allow, no per-request ZDR restriction and no provider order. Custom order is validated as unique lowercase provider slugs before durable admission.

## 118. Reasoning continuation contracts are route-specific and may be structured
- OpenRouter normalizes reasoning requests as `reasoning.effort`, but continuation may return plaintext `reasoning`, its `reasoning_content` alias or ordered `reasoning_details` objects. Treating it as DeepSeek's string-only rule would discard encrypted/summarized/signature-bearing state needed after tool results.
- Chat now preserves the exact detail object sequence in provider state and replays it unchanged. Raw alias choice is retained; switching aliases mid-stream or sending both in one delta fails. Detail arrays are object/type/size/count bounded and provider-controlled values do not enter diagnostics.
- GLM-5.3-Flash catalog evidence is mandatory reasoning with exact max/high/low and default max. Neutral assistant tool history cannot satisfy the continuation contract; provider state with tool calls needs nonempty raw reasoning or details, and response-side missing state emits neither provider state nor Finish. DeepSeek remains independently `reasoning_content`-only.
- Advertising `InferenceAdapter` converts catalog presence into a persisted-profile dependency. Schema v13 adds `catalog-openrouter` only when an older exact config selects OpenRouter, then materializes its `http`/`models` dependencies. Injecting it into unrelated DeepSeek profiles would violate exact-profile intent.

## 119. Native-tool routing is request provenance, not a wire-time preference
- A logical capability such as `web_search` may have provider-hosted, client and MCP implementations. Selecting one only while serializing a provider body would make replay, audit and fallback behavior depend on the current registry rather than the committed request.
- N01 publishes one effect-owned `NativeToolRegistry`. Matching provider-native candidates win, then client, then MCP; priority and implementation id make ties deterministic. Provider candidates cannot claim a different request provider, logical ids are unique/sorted, and stale registrations disappear with their Context effect.
- The Agent resolves routes before adapter resolution, `ResolvedCall` retains them, `request/header.options.native_tool_routes` commits them, and C05 compares them byte-semantically before transport. Client web tools contribute candidates only when the corresponding Consumers exist. Schema v14 repairs older exact profiles before `tools`, `agent` or `subagent`; hidden defaults inside execution would violate both composition and replay contracts.

## 120. Normalized server-tool events are an inspection plane, not replay state
- Provider blocks may contain opaque/encrypted continuation material that must be replayed exactly, while UI/runtime Consumers need bounded call/result/source/citation facts. Converting one representation into the other loses either provider correctness or display safety. Keep exact `ProviderStateItem` data as the model path and log separate normalized events that never feed `derive_messages`.
- A server result can settle a call from an earlier request after `pause_turn` or a mixed client/server-tool stop. Correlation is session-wide but route-stable: call ids insert once, results settle once, output indexes cannot regress within a request, and orphan/duplicate/cross-provider results fail before any output event appends. Raw input and cited excerpts stay redacted from Debug; normalized results retain only outcome/count/safe code/public sources.
- `FinishReason::Pause` is permission to continue only when a strict adapter committed validated exact state. Legacy/state-free Pause is an error. The next request reprojects that state, and repeated provider-owned continuations are capped at eight so a remote loop cannot hold one user turn forever. Failed/cancelled streams publish neither exact nor normalized pending output.

## 121. A server tool's documented observable surface may be weaker than its internal loop
- OpenRouter's current Chat server-tool request is `tools:[{type:"openrouter:web_search",parameters:{...}}]`; the older `plugins:[{id:"web"}]` and `:online` paths are deprecated. Route selection must remove the losing client `web_search`, add the exact provider definition and an explicit `max_tool_calls`, and disable retry because the gateway may already have executed billable work.
- Chat streaming exposes standardized `url_citation` annotations and aggregate `usage.server_tool_use.web_search_requests`. It does not document per-search ids, queries or result blocks. A synthetic call id or guessed empty input would make an aggregate look lossless and break audit semantics. Preserve exact annotations, validate the count/budget, and leave POR05 active until authenticated raw evidence or a provider Responses item supplies exact call identity.
- Provider-scoped capability evidence can include a gateway guarantee: OpenRouter documents fallback search for any model, so its catalog route may mark `native_web` Supported even when the upstream model lacks native search. That claim is valid only while the provider-owned candidate/plugin is selected. Schema v15 migrates the contribution; unrelated providers keep the client fallback.

## 122. `only` policy means admission failure, not silent capability disappearance
- Native/local preference is per logical capability, not one provider-wide bool. `prefer-native` and `prefer-local` may select a fallback family; `native-only` and `local-only` must fail before request commit when no eligible candidate exists. Omitting the logical route would make a requested safety/authority policy look like an ordinary unavailable tool.
- Local is still ordered: client implementations outrank MCP, then priority/id break same-family ties. Matching provider candidates are the only native family; a candidate owned by another provider is ineligible. Unknown persisted override ids fail at resolution rather than waiting for an accidental future match.
- The policy plugin owns a validated Settings namespace and updates the registry live after Settings commits. The Agent reads that immutable registry state before every request, so switching to prefer-local restores the client `ToolSpec`, removes `NativeFeature::Web` and changes the durable header/wire together. Shutdown removes the watcher then resets prefer-native. The plugin is optional for intentional exact profiles; absence preserves N01 behavior rather than smuggling in a hidden dependency.

## 123. A web tool is a Consumer; DNS, HTTP and credentials belong to providers
- Putting reqwest construction, `BRAVE_API_KEY`, DDG parsing and SSRF checks inside `web_search`/`web_fetch` made logical tool routing cosmetic: choosing “local” still hard-wired one implementation. WEB01 adds an effect-owned registry/service and exact `web_provider` inventory; model tools now build bounded requests, pass their operation token and render only validated provider results.
- Provider output is boundary data too. Result count, public URL/userinfo, title/snippet/content/type bounds and fetch truncation are revalidated after dispatch. Errors/Debug contain no query, URL, body, credential, DNS or reqwest text. A provider returning too many or malformed results cannot make them model-visible.
- Parse URLs into typed hosts before SSRF classification. `host_str()` left IPv6 brackets in one path, so `[::1]` missed string-to-IP parsing and became a generic network failure. Match `Host::Ipv4`, `Host::Ipv6` and `Host::Domain`; check every resolved domain address. Bodies stream only to cap+1. Redirects are separate authority transitions and must repeat admission before following.

## 124. DNS admission is incomplete unless the admitted address is the one connected
- Resolving a hostname, rejecting private answers and then letting the HTTP client resolve it again leaves a DNS-rebinding gap. WEB02 resolves each fetch authority once, denies empty or mixed public/private answers (including IPv4-mapped IPv6), and pins that exact approved address set into the no-redirect reqwest client. Direct metadata/private literals never invoke DNS.
- Redirects are fresh authority decisions: resolve relative locations, reject scheme/userinfo drift, detect loops, cap at five, drop the prior response and repeat DNS admission before sending the next request. No request header or client is reused across hops. Search endpoints are trusted provider configuration, but automatic redirects are still disabled so a custom Brave credential header cannot cross an unreviewed origin.
- Cancellation cannot justify dropping a task handle. A spawned blocking DNS lookup detached when cancellation won; use a cancellation-selectable resolver future with no caller-owned `JoinHandle`. Tests pin the mixed-answer, metadata, redirect, loop, hop-cap, credential-redirect and no-detached-handle contracts.

## 125. Provider selection and domain policy must describe the same operation snapshot
- Registration order and priority are deployment accidents, not user policy. WEB04 selects the configured search/fetch id when it is registered, capable and locally available; with no id, exactly one usable provider may auto-select. Zero is unavailable and two is ambiguous. The capability report retains those distinct states so `/web` can explain recovery rather than silently choosing.
- Domain rules are canonical base domains matching exact hosts and subdomains, with block precedence. Suffix checks require a dot boundary, FQDN trailing dots normalize, duplicates/conflicts fail and lists are bounded. Search results are validated then filtered before becoming model-visible. Fetch checks the initial URL, passes the immutable operation policy to the provider for every redirect, and independently verifies the provider's final URL.
- Settings validates persisted provider ids against the composed operation catalog and watchers replace policy only after the durable commit. Visibility for an optional service cannot be an undeclared lookup inside `/status`: separate plugin `status-web` explicitly injects `web`, owns `/web`, and disappears cleanly from exact profiles that omit the capability.

## 126. Content addressing proves identity only when publication and reads re-verify it
- ATT01 hashes exact admitted bytes into a validated `sha256-<hex>` newtype, but the hash alone does not secure storage. The Unix provider holds an owner-only capability root, schema marker and fixed lock file; a same-process gate plus cross-process `flock` serializes publication. Bytes fsync to a `0600` temporary, hard-link no-clobber to the digest path, unlink the temporary and fsync the directory. Existing objects must equal the candidate; every read rechecks regular-file/single-link/mode/identity/length, SHA-256, sniffed MIME and image dimensions.
- MIME is boundary evidence, never a filename claim. PNG/JPEG/GIF/WebP are content-detected and dimension-read without full pixel decode, then bounded to 32,768 per side/100 million pixels. PDF, UTF-8 text and opaque binary have distinct canonical types. Claimed type mismatch, unsafe basename, empty/>configured bytes and corrupt images fail before storage or session publication. Content ids and names stay redacted from Debug/errors.
- Durable ordering is bytes first, then v2 `attachment/added`; the synchronous session bus can therefore read the object during publication. A failed/cancelled append may leave an unreachable immutable object, never a phantom event. Active-operation leases—not a held read lock—let reentrant listeners run, while shutdown cancels first and waits quiescent. Non-Unix storage remains `UnsupportedSecurity` until an audited owner-only backend lands; cross-compilation is not native security evidence.

## 127. Readable extraction is a bounded processor, not a transport side effect
- Raw retrieval and readable model output need separate ceilings. WEB03 lets portable transport retain at most 4 MiB while the tool asks for 64 KiB readable text. A `web_processor` is an effect-owned contribution selected only when exactly one processor supports the declared/sniffed content; provider order never chooses. The provider holds a weak processor handle, avoiding a registry→provider→registry Arc cycle.
- PDF parsing is hostile CPU/decompression work. lopdf 0.44 loads with a 2 MiB decompression ceiling, extracts each of at most 256 pages through its bounded API and stops at the UTF-8-safe output cap. The worker has one JoinHandle, observes caller/plugin/deadline cancellation and is always joined—even a 10-second timeout cannot detach blocking work. Malformed/encrypted/truncated/bomb-like PDFs fail before attachment/session commit. HTML rendering is bounded and script/style content stays out; unknown textual types fall back conservatively while unsupported binary is never lossy-decoded.
- Source provenance must not exist only in a live return value. Extraction constructs one validated URL/title/retrieval/source-truncated/page-count record, stores it beside the raw content address in `attachment/added`, and builds `WebFetchSource` from the same typed facts. The model-visible tool result logs an escaped Markdown source link and page count but never the content hash. A failed extraction creates neither raw attachment event nor citation fiction.

## 128. An admitted image is not model input until selection, capability and wire shape agree
- `attachment/added` proves immutable bytes exist; it does not say which user message may use them. ATT02 adds a separate v2 `user/attachments` event that must reference exact prior metadata and sit immediately before `user/message`. Both JSONL lines are buffered, flushed and only then published. Append, open, projection, compaction and forks all reject an orphaned selection or a boundary that splits the pair — `append` through `advance_pairing` at the commit point, which is why the invariant is refused rather than merely detectable after the fact. TUI pending state therefore clears on the committed attachment echo, not when `/attach` merely admits bytes.
- Model capability evidence cannot repair an unrepresentable payload. Before the user pair is appended, Agent rereads/hash-checks every unique image, constructs the four-format bounded `ChatImage`, requires a strict adapter, refreshes the selected catalog row and accepts only `image_input=Supported`. Unknown is a named refusal, not false or hopeful support. Responses emits `input_image` data URLs, Chat emits `image_url` parts and Anthropic emits base64 image blocks before text; metadata or filenames never substitute for exact bytes.
- Catalog refresh is intentionally registry-owned single-flight work. Cancelling one image turn must cancel and join that caller's wait, but must not abort a refresh shared by another consumer; the registry lifecycle remains the refresh task owner. Tests prove the cancelled turn writes no `user/attachments`, message, draft or transport, while a later waiter can consume the same eventual generation. Do not describe a cancelled waiter as owning or settling the shared provider fetch.

## 129. Document fallback is a durable route decision, not a serializer guess
- A protocol family accepting “files” does not prove that the selected provider/model accepts a PDF. ATT03 adds independent tri-state `document_input` evidence and selects a native route only for exact Supported plus an advertised strict adapter. Responses, current Chat and Anthropic each have different evidenced PDF blocks. Unknown/Unsupported/legacy does not fail or pretend native: it chooses the composed bounded extractor and records that choice.
- Extraction must be reusable without fabricating a web URL. `web-extract` now provides an effect-owned `document-extractor` service that shares classification, lopdf decompression/page/output limits, panic containment, deadline and always-joined worker. Local use returns text only and publishes no provenance. The Agent separately admits that exact text as immutable `text/plain`; web use still admits the raw source with public HTTP metadata. One parser implementation therefore has two honest commit owners.
- `user/attachments` carries `DocumentInputRoute`: native requires identical PDF source/selected metadata; extracted requires exact prior PDF/HTML source plus distinct prior derived text selected metadata. The route sits beside the immediately following message, survives replay/compaction/fork and drives both live mapping and C05's independent reconstruction. TUI labels native versus local extraction. Never infer the route later from the current catalog, file extension or MIME alone; those facts can change while the durable decision cannot.

## 130. Untrusted-content labels belong to the result type, not a tool-name heuristic
- Checking `call.name == "web_fetch"` inside Agent would special-case one built-in, miss `web_search`, aliases and future external implementations, and violate plugin ownership. WEB05 adds a default-none `Tool::untrusted_content` contract; Web Consumers return the typed core boundary. The guarded pipeline attaches it only to successful output, while denials and body-free tool errors remain ordinary safe local text.
- A UI-only badge does not protect replay or a second provider call. Agent commits the boundary on v2 `tool/result`; neutral session projection carries it, and both live request construction and C05's independent reconstruction apply one deterministic model-visible warning around the exact content. V1 ordinary tool results remain readable but a v1 line claiming the field fails. The TUI card and native runtime notice derive from the same typed fact after commit.
- “Untrusted” means data has no instruction, authorization or approval authority; it does not mean the model will reliably ignore every injection. The wrapper deliberately preserves the useful external content and a hostile source may imitate delimiters. Tool schemas, approvals, sandbox and trust gates still enforce effects. QSEC05 must evaluate indirect-injection behavior after MCP12 can classify MCP results; do not market the marker itself as prevention.

## 131. An ACP prompt cannot be cancellable while the reader awaits it inline
- The original stdio loop awaited `session/prompt` inside request dispatch, so it could not read `session/cancel` until the turn had already finished. X03 gives one fair loop ownership of input/output and a JoinSet ownership of prompt tasks. Each ACP session stores the exact active cancellation generation and native RuntimeSession; cancel notifications remain responsive, overlapping prompts fail, and the original request returns `stopReason:cancelled` only after the Agent's durable abort settles.
- Holding only Arc service handles and dropping the composed Context orphaned lifecycle authority. ACP sessions now retain Context plus their runtime/store/Agent, and own the approval forwarder. EOF cancels prompt tokens, denies pending approval waiters, drains prompt handles, cancels/joins the approval task, closes the runtime and only then unwinds Context. The output writer is the main loop, not a detached task. A duplex hanging-provider regression proves both cancel response and half-close teardown.
- Rich UI output should consume the normalized runtime plane instead of separately interpreting UiEvent and session timing. X03 filters replay by monotonic sequence, waits for terminal settlement, and maps committed tool/usage/plan/untrusted notices to ACP v1 shapes. User content echoes wait for `TurnStarted`, proving the user message/attachment pair committed first. Image/resource bytes enter through `RuntimeInput` and ATT01 rather than an ACP-only media store. A bounded 48-MiB UTF-8 line reader is required because advertised 32-MiB media would otherwise make unbounded JSONL framing the weakest boundary.

## 132. A local client still needs a real versioned wire boundary
- Calling Agent directly behind a Rust trait would let the TUI compile but would not prove JSON-RPC stability or future SDK compatibility. X04's `LocalAppClient` serializes every request, parses every response, and receives events only after each typed notification serializes/deserializes through protocol v1. Closed methods and event variants own casing, ids, error codes and contiguous app-server sequence independently from internal RuntimeEvent evolution.
- The stable backend should not spawn a second turn task. The TUI already owns one supervised foreground JoinHandle and cancellation token, so `AppServer` awaits the native RuntimeSession inside that owner and streams through a bounded channel. The service lazily starts/resumes the current session, filters runtime replay, and reads the committed user input/attachment route from the session log at TurnStarted. A dropped event receiver does not drop admitted model work; TUI cleanup cancels/joins the turn, closes the local client, then Context shutdown cancels the service before runtime/Agent effects.
- Replacing TUI's turn control introduces a persisted dependency even when other dialogs stay local. Plugin `app-server` follows `runtime-native`, TUI declares/injects it, schema v17 inserts it immediately before historical TUI rows after existing runtime/routing prerequisites, and exact composition tests cover the service owner/type. During client-owned foreground work, duplicate direct assistant/user lifecycle UiEvents are filtered; richer local tool/dialog/command values remain direct until X05 publishes their full control surfaces. This preserves current diff/untrusted cards rather than degrading them to generic protocol text.

## 133. Control protocols must not turn inspection into secret access

- Keep the base turn host and broad control plane as separate effects. X05's optional default `app-server-controls` plugin injects owner services, contributes exact method inventory, installs one token-owned control generation into `AppServer`, and removes it before the base service. Intentional minimal profiles can retain turns without falsely satisfying auth/model/MCP/settings dependencies.
- A credential “status” list is not necessarily credential-blind. The native keyring API has no metadata-only existence operation, so `CredentialsService::describe` may retrieve and immediately zeroize a password internally; on a locked macOS keychain that call can also block. App-server authorization catalog listing therefore returns explicit `inspected:false` metadata and never calls a credential backend. Only registry-owned authorization commit/readback returns authoritative configured/source/writable state.
- Interactive authorization needs two correlations: the app operation newtype selects only its own broker notification, and the broker prompt id accepts an answer/cancel. The server additionally binds a flow's non-secret reference to the currently effective provider profile before prompting. A drop guard removes a pending broker row and emits a safe unresolved settlement when the prompt future is cancelled or dropped, preventing a stale client answer from committing later.
- Generic settings projection must fail closed. `SettingsSchema::with_wire_exposure` is an explicit owner attestation; absent it, app clients see only namespace/revision/application timing, never schema/default/base/user/project/resolved values, and cannot replace the section. Exposed replacement still goes through expected-revision durable CAS and owner watchers before publication. Model fallback likewise keeps an unproven current id visible but unselectable and marks only the provider-owned default selectable.

## 134. An SDK must own one wire contract without importing the host

- Put the public protocol and transport-neutral client below the server. X06's `heycode-sdk` depends only on core plus serde/async utilities; `heycode-app-server` consumes and re-exports those exact types and implements `AppTransport` for its local server. Making the SDK depend on the server would compile but force every client to import Agent/runtime/credential/MCP host code and leave two likely-to-drift client implementations.
- A transport exchange owns raw request/notification/response frames. The SDK caps each at 4 MiB, correlates response ids, decodes the closed event union, validates session identity and requires notification sequence contiguity within that operation. The first sequence is intentionally arbitrary because the server sequence is global and another request may have consumed earlier notifications. When selecting between a response future and a notification receiver, disable the receiver branch after `None`; a biased closed channel is perpetually ready and otherwise starves an already-complete response forever.
- `start` and `resume` cannot choose a session path behind a remote host. Host composition owns new/resumed session selection; `start` retains the returned identity, while `resume(expected_id)` additionally refuses silent attachment to a different host-selected session. Turn streaming uses caller-owned bounded delivery, and concurrent cancel uses a clone whose future is also awaited—no SDK task is detached.
- TypeScript compile-time types are not boundary validation. The TypeScript 7 client runtime-checks JSON-RPC envelopes, every closed event, route/auth/model/plugin/settings responses, safe integers, 128-level JSON depth, session ids and sequence. A pinned lockfile, runnable Rust/TypeScript examples and one shared v1 fixture catch casing/option/event drift across languages. Raw transport frames are never logged because authorization answers transiently contain a credential.

## 135. Current official docs do not replace a pinned delegated schema

- Research the current official method semantics, then generate the exact installed CLI schema. Codex 0.146.0 uses stable `account/read`, `model/list` and `modelProvider/capabilities/read`, but pinned response details already differ from current documentation (for example its Bedrock boolean is `usesCodexManagedCredentials`). R04 records the aggregate schema and individual response hashes; upgrading the executable requires regenerating and reviewing them rather than silently accepting today’s page examples.
- Credential-blind means the heycode boundary never resolves or parses token values. `account/read` always sends `refreshToken:false`; ChatGPT email is shape-validated then discarded, while the safe label contains only the closed plan id. API-key and Bedrock modes get static source labels. A null account with `requiresOpenaiAuth:true` is Disconnected; false is the distinct `AccountStatus::NotRequired`, not a fabricated Connected account.
- Each account/catalog operation owns a fresh pinned app-server connection and closes it after success, protocol failure or caller cancellation. Close uses an uncancelled teardown token and must settle the reader/input/contained process before the runtime method returns. Plugin registration remains side-effect-free; descriptor `models=Supported` advertises the callable method, not eager discovery.
- Model discovery reads provider capabilities once, then follows `model/list` cursors with 100-row, 64-page and 4,096-total bounds plus cursor/id/alias deduplication. Hidden rows remain absent. Effort ids/defaults and modalities are structurally validated; normalized reasoning/image evidence comes from those exact fields. `webSearch` maps both true and false, while `namespaceTools:true` proves tools support but false remains Unknown because it does not prove Codex lacks all non-namespace tools. `imageGeneration` is validated but not misreported as image input. Upgrade ids become explicit deprecation replacements; other lifecycle/limits remain Unknown.

## 136. A pinned delegated protocol needs a closed notification union, not a partial one

- Pinning a delegated runtime's schema is only half the contract. R06 handled fourteen Codex `ServerNotification` methods and ignored thirty-one, but the pinned `codex-cli 0.146.0` union has **seventy**. The remaining twenty-six fell to the catch-all `_ => Err(protocol())`, and because a driver-task error is terminal the whole primary session died. `thread/name/updated` alone made this near-certain in real use: Codex auto-names a thread right after its first turn. `mcpServer/startupStatus/updated` and `skills/changed` fire for any user with MCP servers or local skills.
- Fail-loud applies to **version drift**, not to pinned methods you chose not to model. Separate the two states explicitly: a method outside the pinned union is a protocol error naming real drift; a pinned method carrying no state you own is an explicit, listed ignore. Collapsing them into one catch-all converts every unmodelled-but-known event into a user-visible session kill.
- Prove the closure with a test, never by reading. Keep the handled and ignored sets as consts, pin the generated `ServerNotification` union as a test fixture, and assert `handled ∪ ignored == pinned` with both differences reported by name. Add a source-law check that every handled name appears in the dispatcher body and no ignored name does, so the lists cannot drift from the `match`. Generate the fixture with the official tool (`codex app-server generate-json-schema --out <tmp>` under an empty `CODEX_HOME`) rather than transcribing documentation.
- The same reasoning applies to server→client **requests**, but with the opposite default. There the safe behavior is to answer only the exact pinned human-interaction requests and fail everything else loudly: `mcpServer/elicitation/request`, `item/tool/call`, `attestation/generate` and especially `account/chatgptAuthTokens/refresh` must never be serviced, because answering a token refresh would end credential-blindness. Silence is not an option either — an unanswered request would hang the runtime — so terminate the connection instead.
- A test-only const is production dead code. `PINNED_HANDLED_NOTIFICATIONS` exists solely for the audit, so it lives inside `#[cfg(test)] mod tests`; leaving it in the module body fails `-D warnings` on `dead_code`. The ignored list stays in production because the dispatcher consumes it.

## 137. Queue wake rules must come from lifecycle state, and a settling turn must re-drain

- A03's three deliveries answer three separate questions, and conflating them loses input. *Where* it queues is `InboxDelivery::target` (follow-up → next turn; steer and inject → next step). *Whether submitting wakes an idle agent* is a different axis: follow-up and steer wake, inject deliberately does not. *When it becomes model-visible* is a third: only a claim makes it so.
- Never infer busy/idle from a spinner, a UI flag or a duplicated bool. `AgentCancellation` already owns the authoritative lease between `begin_turn` and the drop of its `TurnCancellation`; `is_turn_active()` exposes exactly that. A second copy of the flag drifts the moment a turn ends on an error path.
- "Busy ⇒ never wake" is only safe if the running turn genuinely cannot strand the work. Draining at the top of each step is not enough: input arriving after the last step boundary would be claimed by nobody. The turn therefore **re-drains immediately before settling** and, if anything was claimed, runs another step instead of ending. Without that second drain the wake rule is a silent message-loss bug rather than an optimization.
- A follow-up queued while busy is still owed a turn, and the agent must not start one itself — that would race the caller's own turn ownership. Settlement instead publishes the pending counts with an explicit owed wake, so exactly one owner starts exactly one turn. Publishing happens after the durable close, never before.
- The claim is the commit point, so the removal splice and its `user/message` must be one atomic append. `append_kinds_atomically` originally skipped the inbox validate/commit path entirely — safe only because its two callers were attachment pairs. Any stateful projection must be threaded through the batch path too: validate each event against the state its predecessors produce, and swap the projection in only after the whole batch is durable.
- Claim one message per atomic pair rather than N removals plus N admissions. An interrupted drain then leaves a consistent prefix claimed and the remainder pending, which replays correctly; a partially written N-event batch would not.

## 138. A provider contract must separate seed from continuation, and the registry must own child lifetime

- O01's predecessor encoded delegation as one `fork: bool` plus three ad-hoc spawn methods, which silently coupled two independent questions. *Seed* (`Fresh` vs `ForkParent`) is where the child's context comes from; *continuation* (`OneShot` vs `Continuable`) is whether it survives its first turn. "fork" happened to mean fork+oneshot and "continuable" happened to mean fresh+continuable, so fork+continuable was simply unreachable. Model the axes separately and the missing combination becomes a capability question rather than a hole.
- Capability evidence is tri-state and `Unknown` never counts as support. `SubagentProviderDescriptor::supports` requires exact `Supported` for a fork seed or a continuable request, so a provider that has not proven a mode is skipped during selection rather than being handed a request it will mangle. Selection is deterministic registration order, never "last registered wins".
- The registry, not the runner, owns live children. The old runner kept a `HashMap` that nothing ever removed, so every continuable child leaked its `Arc<Agent>` and durable session for the life of the process. One context effect now disposes providers *and* interrupts every live child, so shutdown cannot strand a child holding an open session. Disposal interrupts rather than awaiting close, keeping the disposer synchronous.
- `ctx.provide(key, owner, value)` stores the value; passing an `Arc<T>` stores `Arc<Arc<T>>` and every `ctx.get::<T>(key)` then returns `None` with no compile error (GOTCHAS #27 again, from the other direction). Provide the bare value, then `ctx.get` it back when you need the shared handle.
- Migrating onto a contract is proven by the *unchanged* tests. All eight pre-existing subagent tests passed without edits, which is the actual evidence for "migrate without behavior loss"; the new tests then pin only the new surface. Rewriting the old tests to match the new code would have destroyed that evidence.
- A follow-up must continue at the child's real depth. The old `send_message` scoped `TASK_DEPTH` to a literal `1`, which is correct only when the parent was top-level, so a nested continuable chain could reset the nesting count. The handle now records `request.depth + 1` at creation. The complete ownership/authority model is still O03.

## 139. An open delegated union inverts the closed-union rule, and the shipped binary is a primary source

- GOTCHAS #136 said a pinned protocol needs a closed notification union. Claude Code is the opposite case and needs the opposite default. Its `SDKMessage` union is **explicitly open** — 35 documented variants, 95 `subtype` literals in the shipped 2.1.250 bundle, and documentation stating new members may appear. Erroring on an unmodelled top-level `type` would kill a healthy session the first time Claude emits a task notification or a hook event. So R09 ignores unmodelled stdout messages by default while keeping envelope shape, session identity, correlation ids, control-request subtypes and payload bounds strict. Decide the default from whether the protocol is *closed and versioned* (Codex) or *open and evolving* (Claude) — never by habit.
- Documentation gaps close from the shipped implementation. The published Claude docs never print the control-protocol wire literals; `strings` over the installed binary does, and it is the actual authority: envelope types `control_request`/`control_response`/`control_cancel_request`/`control_request_progress`, plus subtypes `interrupt`, `can_use_tool`, `request_user_dialog`, `set_model`, `set_permission_mode` and 90 others. Reading the shipped bundle is credential-blind, offline and exact — prefer it over inference from SDK method names.
- `AllowSession` is not the same authority everywhere. Claude's allow path can carry `updatedPermissions`, whose Bash suggestion has a `localSettings` destination that **writes a persistent rule into the project's settings file and outlives the session**. heycode's "allow for this session" is strictly narrower, so both allow decisions map to a bare `{behavior:"allow"}` and `updatedPermissions` is never sent. Mapping a permission vocabulary across runtimes requires comparing *scope and durability*, not just the label.
- Never service a credential request. The CLI can originate `oauth_token_refresh` and `host_auth_token_refresh` control requests; answering one would end credential-blindness and staying silent would hang the runtime, so the session fails loudly instead — the same rule R06 applies to Codex's `account/chatgptAuthTokens/refresh`.
- `parent_tool_use_id` is correlation, not decoration. A non-null value means the frame came from a delegated subagent whose `tool_use` ids belong to a nested context; emitting them as this session's tool calls desynchronizes the turn against its own results. The dead-code lint on that field is what surfaced the bug.
- Do not project data a consumer already has. Complete assistant `text`/`thinking` blocks duplicate the streamed deltas and the `result.result` final text, so the parser drops them and `AssistantBlock` retains only `ToolUse`. `-D warnings` dead-code analysis is a reliable detector of exactly this class of redundant parsing.
- Claude has no synchronous turn ack, so the **host-minted message uuid is the turn id**. It returns as `user_message_uuid` and names the turn in an interrupt receipt. Likewise `--session-id` lets the host choose identity up front, so `system/init` is *verified* against an expected value rather than trusted — except on fork, which deliberately reports a new id.
- `shouldQuery:false` is the documented append-without-turn primitive, which is why Claude can support `follow_up` while Codex cannot. Compaction has no control subtype: it is the `/compact` slash command, and only an observed `system/compact_boundary` proves it happened — the command settling is not evidence.
- `spawn_interactive` requires `ProcessSpec::with_interactive_stdio()`; without it the local provider rejects the spec as `InvalidSpec`, which surfaces far from the cause.

## 140. A raw HTTP boundary must not follow redirects, and a missing price is not free

- reqwest's default redirect policy follows up to ten hops and strips only `Authorization` across origins. Custom headers survive, so an automatic hop re-sends protocol identity (an MCP session id) and any provider key carried in a custom header to whatever host the redirect named. `heycode-http` now builds with `redirect::Policy::none()`: a 3xx reaches the caller, which owns per-hop policy exactly as the portable web provider already did. A raw provider-neutral transport should never make a second request the caller did not ask for.
- A response header map is protocol surface, not decoration. Protocols whose identity lives in a header (MCP's `Mcp-Session-Id`) are unimplementable without it. Bound it: 64 headers, 4 KiB per value, names lowercased, and drop non-UTF-8 or oversized values rather than truncating them into something a parser would misread.
- Adding a required field to a public struct with public fields is a deliberate, useful break. `ModelDescriptor` gained `pricing` and `performance`, and Rust forced all 21 construction sites to state their evidence explicitly. An `Option` with a `None` default would have let every provider silently keep publishing "no price" without anyone deciding that. Take the break; it is the compiler doing the audit.
- "Missing" and "zero" must be distinguishable at the type level, because a zero price reads as free. `ModelPricing` stores a component map, so an unpublished component is absent rather than zero-valued, and the wire format omits the whole pricing object instead of writing zeros. OpenRouter publishes `prompt`, `completion`, `input_cache_read`, `input_cache_write` and `internal_reasoning` per token; its media components (`image`, `audio`, `web_search`) are deliberately unmapped because they are not per-token and inventing a component for them would misreport cost.
- Persist exact integers in the published unit, never floats and never converted. Prices are stored as pico-units (10^-12 currency units) alongside their currency and unit, so `0.000000075` USD/token round-trips as exactly `75000` with no drift. An unrepresented currency, unit or component in a newer file **fails the read** rather than degrading to "no price"; that required making the catalog-file conversion fallible all the way out to the store, which is the correct shape for an explicit wire mapping.
- A blanket textual patch across construction sites will also hit type positions — `-> HttpResponse {` and `-> ModelDescriptor {` look identical to a struct literal. Verify with a build rather than trusting the substitution, and de-duplicate before assuming success.

## 141. Background work must be wake-budgeted, and a dynamic tool must still be declared

- Background jobs are the natural way to spin an agent forever: every settlement wants attention, and N jobs settling means N wakes. O04 makes waking a budgeted resource. A settlement whose delivery would wake an idle agent spends one token; with none left the notice is **demoted to a non-waking delivery, never dropped**. Tokens replenish when a turn settles, so a job storm costs at most one wake per turn rather than one per job. Losing the notice would be a correctness bug; losing only the wake is the intended back-pressure.
- Replenish *before* announcing settlement. A job that settles inside that window can then still wake exactly one turn, instead of being demoted by a budget that had not yet refilled.
- Settlement is exactly-once and must be enforced by state, not convention. `settle` refuses a job that is not `Running`, so a duplicate settlement cannot deliver a second notice or spend a second token. Cancel, by contrast, is idempotent *while running* and returns false once settled — those are different questions and conflating them produced a wrong test assertion before it produced wrong code.
- The registry owns every task and token, and disposal cancels and aborts them synchronously rather than awaiting — a context disposer cannot block. This is also why the caller hands over its `JoinHandle`: nothing is ever detached.
- A dynamically registered tool must ALSO be declared in `Plugin::inventory()` with the matching family in `descriptor()`. `RealCompositionHarness`'s exact audit compares declared inventory against the live registry, so registering a tool without declaring it fails composition evidence — which is precisely the attribution guarantee `/plugins verbose` depends on. The audit caught it; do not "fix" it by editing the fixture.
- Do not add a public API purely so a test can reach production state. An `install_jobs_for_test` shim was the wrong instinct: the composed world already provides the registry, so the test should resolve it from the context — which also makes the test prove the production wiring rather than a hand-built stand-in.

## 142. A registry with no service key is a library type, and evidence classes want separate types

- P11 shipped `TokenCounterRegistry` with no service key and no plugin, so nothing in a composed world could reach it. A registry that is not mounted is not a registry — it is a struct. Marking that row complete would have been the same hollow completion as a transport with no configuration producer. The fix is a plugin that provides the service *and* registers the always-available fallback, so `list()` shows exactly what can answer a request instead of hiding a default inside the lookup.
- Separate evidence classes deserve separate **types**, not one enum a `match` can flatten. `ExactTokenCount` and `EstimatedTokenCount` are distinct structs minted only by the registry from a counter's declared evidence; a consumer that requires a measurement asks for the exact type and an estimate cannot coerce into it. An `is_exact()` boolean on one shared struct would have been forgettable at every call site.
- Deterministic tie-breaking must come from the data structure, not from a comparator comment. Counters live in a `BTreeMap` keyed by id, so `.values()` is id-ordered; Rust's `sort_by_key` is stable, so sorting by evidence rank preserves id order *within* a rank. That is what makes a tie break by id rather than by registration order — a `HashMap` would have silently made the same code non-deterministic.
- A counter that cannot honestly count must refuse, distinctly. The Anthropic counter now reconstructs current structured tools/results/images/PDFs exactly enough for its endpoint, but still refuses anything outside that represented set and an empty transcript (the endpoint rejects it, and zero would be a lie), rather than dropping content and under-counting. Its result is `Estimated(ProviderTokenizer)` because the provider documents the endpoint as an estimate. Under-counting or mislabeling evidence is worse than refusing.
- Verify a delegated lane's *facts*, not just its gate. PAN01's model-list field names were the risk: `max_input_tokens`, `max_tokens` and `capabilities` are the real Models API fields (there is no `context_window` field), `claude-opus-5` is a real id, and Anthropic publishes no pricing endpoint — so `ModelPricing::unknown()` is correct and transcribing a documentation pricing table into code as if it were API evidence would not be. Checking those against an authoritative source took minutes and is the difference between shipped evidence and shipped plausibility.
- Two mechanical traps worth remembering: a `python` string replace that silently matches nothing will happily "succeed" — assert the anchor exists; and inserting a function immediately before another function's signature splits it from its doc comment and `#[must_use]`, which surfaces as a confusing duplicated-attribute lint rather than a misplacement error.

## 143. A rank reserved for a component that does not exist is still speculation

- P11 originally added an `EstimationMethod::ModelVocabulary` variant with no producer, so the evidence ladder had a speculative rung. PAN06 is the event that justifies adding a real one: Anthropic now documents `/count_tokens` as an estimate, and a working counter produces it. `ProviderTokenizer` was added then—not earlier—forcing every match/ranking test to be revisited. It sorts ahead of `Utf8ByteRatio` but still mints `EstimatedTokenCount`; provider-operated never implies exact.
- A test that still passes when you delete the code it names is pinning nothing. Mutation-test the subtle rules, not just the obvious ones: deleting an evidence sort, no-oping a disposer, or making a failure fall through should each turn specific tests red. If one does not, the test is decoration — rename it to what it actually proves, or replace it.
- When a delegated lane volunteers a limit it could not close, that is signal, not weakness. Three of the most valuable findings this session came from lanes reporting what their own work does **not** guarantee: an enum a consumer can flatten, a guard no test observes, and a heuristic whose error runs in the dangerous direction. A lane that reports only green is harder to review, not better.

## 144. A context meter that sums an unknown as zero tells you there is room when there is not

- C11's envelope keeps contributors separate — system, messages, tools, provider state, attachments — because a single total hides the only question worth asking. "12,000 tokens" is useless when 9,000 is provider state you cannot drop and 2,000 is an attachment nobody counted.
- Two arithmetic rules keep it honest, and they are CAT06's price rules applied to tokens. **A total is only as good as its weakest contributor**: exact plus estimated is estimated, never exact. **Unknown is not zero**: a contributor nothing could count makes the total an `AtLeast` lower bound, and there is deliberately no plain `u64` total accessor, so a caller must match the variant and decide rather than be handed a number that looks complete.
- The dangerous direction is under-counting. P11's heuristic already errs low on dense text, and attachments are genuinely unmeasurable without a vision tokenizer — so an attachment must appear in the envelope as `Uncounted(Unmeasurable)`, not be omitted. Omitting it and summing zero is the same bug wearing a different hat.
- Cost inherits the weakness of the count. An exact published price applied to an estimated token count is **not** an exact cost. And an envelope is request *input*: a model publishing only an output price is `Unpriced`, because substituting one component for another misreports cost — the same "missing is not free" rule as #140.
- Integer division truncates, which is the right direction here: a per-million price applied to a token count rounds the cost **down**, keeping it a lower bound rather than silently inflating it.
- "Nothing to measure" and "could not measure" are different statements. An empty envelope is `Exact(0)`; an envelope with one uncounted contributor is `AtLeast`. Collapsing them would make an empty context indistinguishable from an unmeasurable one.
- Mutation-check the rules that define the row. Three deliberate weakenings — summing `Uncounted` as zero, letting an estimate produce an exact cost, and falling back from the input price to the output price — each turned exactly one intended test red. A rule nobody can break in a test is a rule nobody is enforcing.

## 145. Verify a strategy's transaction; do not trust it

- Compaction is the one operation that deliberately destroys model-visible history, so detecting an inconsistent write after the fact is weaker than preventing it. C12 now makes every strategy return a read-only `CompactionPlan`; the registry brackets preparation with a durable snapshot, rejects any strategy-side mutation or concurrent writer, validates one exact prefix/representation and is the **sole append owner**. `Noop`, cancellation and every failure write nothing; `Applied` writes exactly one portable or native settlement.
- The contract stays observable. A scripted strategy that mutates the session while returning `Noop`, writes twice, proposes a nonexistent boundary, or returns provider state under a portable/prune descriptor produces `BrokenTransaction`. An honest plan writes nothing itself and the registry commits it once. A contract nothing can violate in a test is a contract nobody is enforcing.
- Strategies that fold local history share one boundary rule, not re-derive it. Native, portable and prune cut at the same keep window, its leading `user/message`, and any `user/attachments` immediately before it; only the replacement representation differs. Two copies of that rule would drift, and the drift would only show up as a projection that silently loses a message.
- A lossier strategy must say so in the record. Prune commits a fixed marker instead of a summary, because the projection re-injects that text as model-visible input: the model is told history was dropped rather than handed a shorter conversation it cannot account for. Committing an empty summary would have made a prune indistinguishable from a failed summarization.
- `clippy -D warnings` catches the deadlock class GOTCHAS #2 describes. A `MutexGuard` on the std session mutex held across an `.await` is exactly what breaks `context_estimate`-style code; scope the guard and clone what you need out of it before awaiting.

## 146. An attestation the service verifies, and the boundary that renders it

- S15 inverts `with_wire_exposure()`: it was an owner attestation the service accepted, and is now a **claim the service proves** at every resolution. Exposure holds only when every projected path is classified — declared secret (redacted), attested public, or surviving both a key-name lexicon and a structural credential-material detector. Anything unclassified fails loud and publishes nothing. "The owner said it was safe" is not evidence; "no path in this namespace can carry unclassified material" is.
- **A proof is worthless if the boundary bypasses it.** `heycode-app-server`'s `setting_row` gated on `wire_exposed()` but then rendered the *raw* layers, so the redaction never reached a client. The exposure flag and the values must come from the same place: `exposed` now derives from `wire_projection().is_some()`, and every value field is read off that projection. Writes are gated the same way — a namespace that cannot render values must not accept writes to them.
- A managed lock is a `Conflict`, not `Unavailable`. The write was refused because current administrator policy forbids it; reporting it as a broken service sends the client to the wrong remedy. Adding a new wire error code would have rippled through the TypeScript SDK and the shared fixture for no gain.
- **A security test written against the product world can be decoration.** My first regression test scanned every projected settings layer for credential-shaped markers and passed — but it also passed with the raw-layer bug restored, because no namespace in the default world holds credential material (the credentials namespace holds *references* by design). The distinction was untestable there. The real test builds a purpose-made namespace with a declared secret path, asserts the raw layer still holds the canary (or the test proves nothing), and then asserts no rendered layer does. That one fails when the boundary regresses.
- Redaction must be targeted, not blanket. The test asserts a non-secret sibling still renders: a namespace that blanks itself under suspicion is indistinguishable from one that is broken, and it trains readers to ignore the placeholder.

## 147. A persistent terminal must own no consumer handle, drain continuously, and never close stdin inside a hard kill

- E07's registry returns only an **id** from `open`. There is no handle for a consumer to drop, so there is no way to strand a process group by forgetting one — the registry owns every session, its cancellation token is a child of the registry token, and the disposer is `close()` itself, which is runtime-free and idempotent.
- Drain continuously into a **bounded ring**, never on demand. An unread terminal whose output nobody drains would block its child on a full pipe; the drop-oldest ring keeps the newest bytes, counts what it discarded in `dropped_bytes`, and reports that to the reader rather than silently truncating. A `read` is separately clamped below the retention bound.
- **Do not close stdin inside a hard kill.** The original `kill` closed input first, which delivers an implicit EOF: a well-behaved child then exits cleanly with status 0, turning a documented hard kill into a hidden graceful shutdown that reports success. Kill the tree; let the writer drop with the retired session.
- Reserve the capacity slot **atomically with admission**, not after the spawn. A check-then-spawn leaves a TOCTOU window where concurrent opens exceed the per-owner bound — and a test that never actually overlaps (local spawn has no yield point) will not find it. A failed launch must return its reserved slot.
- Owner scoping is only real if the model cannot choose the owner. The tools bind it from the host at composition; a foreign id is refused **indistinguishably from an unknown one**, so the id is not a capability and its existence is not disclosed. Every operation — read, write, resize, kill — enforces this, not just `list`.
- A test that pins the wrong actor proves nothing. E07's first shutdown test asserted `ServiceStopped`, but the descendant reaping it appeared to prove was actually coming from `subprocess-local`'s disposer; a standalone `close()` test was needed to isolate the registry's own behaviour. Two of its sixteen mutations found defects in its **tests** rather than its code, which is the point of mutation-checking.
- `TerminalSpec::new` requires `ProcessSpec::with_interactive_stdio()` — a terminal is interactive by construction, and omitting it surfaces as a bare `InvalidSpec` far from the cause, the same trap as GOTCHAS #139's `spawn_interactive`.

## 148. Provider facts belong to providers, and a streaming transport that drops headers cannot carry them

- P12's vocabulary deliberately holds **no** provider's header spellings. `heycode-llm` owns `RateLimitScope`, `RateLimitWindow` and the parsing; each provider supplies its own `RateLimitHeaders` mapping. Baking one provider's names into the shared layer would report nothing for every other provider while looking like it worked — the worst kind of failure, because the display stays plausible.
- Only `Retry-After` is read without a declaration, because RFC 9110 is the one genuine cross-provider standard here. Everything else is a provider's private spelling. The verified `anthropic-ratelimit-unified-*` family came from the shipped Claude Code binary, not from a guess; the classic per-quota family is **not** in that binary, so it is not assumed either.
- **Reported and derived costs are different facts.** A cost the provider stated is authoritative; one computed from published prices times reported usage is arithmetic. `RequestCost` keeps them as distinct variants so equal numbers with different provenance are not equal, and a `/usage` display cannot present a calculation as a bill.
- Deriving a cost requires **both** the input and output price. Falling back to the input price for output tokens, or ignoring them, understates the bill — so a partially priced model yields `Unknown`, and `Unknown` has no number at all rather than a zero that reads as free. Per-million prices truncate down, keeping a derived cost a lower bound.
- Advisory telemetry and authoritative data get **opposite** malformed-input rules. A catalog generation rejects everything on one bad row, because a wrong catalog misroutes requests. A rate-limit snapshot drops only the field that failed to parse, because discarding the fields that did parse loses real information for no safety gain. Know which kind of data you are holding before choosing.
- A window with nothing parsed is **absent**, not an empty quota. Publishing a zeroed window would read as "no allowance left" and could make a client back off against a limit the provider never stated.
- `HttpTransport::sse` originally yielded only `SseEvent`s, so a **streaming** inference call could not observe response headers at all — the same gap the buffered path had before MCP03. Twenty-three transports implement that trait across crates other lanes were editing, so the fix was a **defaulted** `sse_exchange` method rather than a signature change: zero impls broke, and only the transport that genuinely observes headers overrides it.
- The default returns an `unavailable` slot, never an empty map. A transport that cannot report headers must say **unknown**; an empty map would read as "the response had no headers", which is a different and false claim. Headers publish for every response that arrived, success or not — a 429's rate-limit headers are precisely the ones a caller most needs.
- Declaring nothing is the correct answer where nothing is verified. `Provider::rate_limit_headers` defaults to none, and no in-tree provider overrides it, because none of their header spellings could be verified from a primary source. A row whose acceptance is "without guessing" must apply that standard to itself: an empty declaration is evidence-shaped, a plausible-looking guess is not.

## 149. Exhaustion must be terminal, retirement must precede the claim, and disposal must cancel what it threaded

- A reconnect budget only bounds a crash loop if **exhaustion is terminal**. If `recover()` re-admits after the budget is spent, any outer Consumer that retries on failure rebuilds exactly the loop the budget existed to prevent. Re-arming is an explicit operator action that constructs a new supervisor, not a state the old one drifts back into.
- **Retire the rows before reporting `Remove`.** A snapshot that says no generation is retained while that generation's tools are still model-visible is a lie the model acts on. Order the durable effect ahead of the claim, as always.
- Cancellation is neither a commit point nor a spent budget. A supervisor cancelled mid-attempt reports `Cancelled{attempts}`, not `Exhausted` — conflating them would burn a budget the operator never spent and, because exhaustion is terminal, permanently disable a healthy server.
- **Disposal must cancel the token it threaded into the transport, not merely abort its own task.** Aborting the supervisor's future leaves a transport still waiting on a live token — and therefore possibly a live child process. This one survived every test until mutation testing found it: nothing observed the token the supervisor hands *outward*.
- A test that passes because the code under test self-terminates is pinning nothing. Two of MCP07's fifteen mutations survived, and both were defects in its tests: one never observed the threaded token; the other never reached the mid-attempt cancellation arm because every test cancelled during a backoff rather than during a connect. A third passed with a no-op disposer because the task ended on its own. Fixing a surviving mutation usually means fixing the test.
- **The supervisor is now armed, through a liveness slot rather than a direct call.** `stdio_definition` builds an enabled bounded policy and `driver_loop` fires the slot on any unrequested transport exit; the slot reports Transport/KeepLastGood first and only then calls `recover()`, so a snapshot never claims health the generation owner has not seen. The slot is armed *after* the generation owner and supervisor exist, and it holds a weak reference to break the connection→supervisor→connection cycle that a strong one would create — a cycle here leaks the transport and its child process for the life of the process. Streamable HTTP keeps a disabled policy deliberately: there is no persistent transport between calls whose death could fire a signal.
- Put the schedule on the policy, not in the supervisor. `delay_before_attempt` is data the definition owns, so backoff is asserted in virtual time (`start_paused`) against exact numbers instead of guessed at with sleeps — and the shift is clamped so a huge attempt number saturates at the ceiling rather than overflowing.
- An **exact error-class assertion driven through a real process is load-sensitive**. `heycode-runtime-claude`'s malformed-output test asserted `Protocol`, and under full-workspace parallelism the fixture exceeded the handshake timeout and legitimately reported `Unavailable` — a true fact about the machine, not the parser. Classification belongs in a deterministic unit test over the parse function; the process-driven test keeps only what must hold on every path, fast or slow: that no provider body escapes.

## 150. Seam scope follows what the seam intercepts, and a short-circuit that changed history must not re-run the chain

- AGENTS §3 said a seam is "stored as a service". A02 needed three seams whose decisions belong to **one turn loop**, and making them global services would have forced a subagent child to inherit its parent's step budget — or pushed every layer into keying state by agent identity. The law was amended rather than bent: a globally-scoped decision (`seam/pre_tool`: one tool registry, one approval policy) is a service; an owner-scoped decision is owned by that owner. Plugin extensibility survives either way, because a plugin resolves the owner's service and pushes a layer onto its chain.
- **A short-circuit that mutated durable history must not re-run the chain.** The request seam's `Rebuild` verdict means a layer changed the log, so the agent rebuilds the request from the log exactly once and dispatches that without re-entering the seam — later layers must never inspect a request whose log no longer exists. Re-running would also let two layers compact in one step.
- A short-circuit before a durable write must leave **no** trace. `StopTurn` decides before `step/start` is appended, so a stopped step never appears in the log at all; the turn still closes `turn/end error` so nothing is left open. Deciding after the append would have recorded a step that never ran.
- One closure per failure. Seven hand-rolled `emit(Error) + close_turn + return Err` blocks collapsed into a single `fail_request`, which is what makes a request-error seam possible at all: a seam needs THE decision point, and seven of them is not one. The seam is deliberately not cancellable — the closure must finish even on a cancelled turn, or a cancelled failure leaves the turn open.
- Mutation testing found two blind tests here too, and the causes are worth knowing. A synthetic layer that **changes nothing** cannot detect a short-circuit that dispatches the stale value — the layer has to mark the thing it claims to have replaced. And `cancel.child_token()` auto-propagates the turn token, so an explicit `operation.cancel()` only matters for the *independent caller* token; removing it survived until a test parked a layer and released it through the caller token specifically.
- A test that waits unbounded on a `Notify` does not fail under a mutation — it hangs, which reads as an infrastructure problem rather than a caught defect. Bound every wait in a test that a mutation is expected to break.

## 151. A usage projection needs no service, and the layer that owns tokens must not own prices

- TEL01's acceptance is that `/usage` works with telemetry **disabled**, so the projection observes no live request, keeps no counter and needs no service. It reads only events already written, which means it is identical on a live session and on one replayed after restart — and projecting the same slice twice yields the same value, which a test asserts.
- The projection stays **neutral**: it reports tokens, instants and the route each turn used, and prices nothing. `heycode-session` sits below `heycode-llm` in the dependency order and must not import it, so a Consumer joins usage to `ModelPricing`. Same boundary the request projection keeps by emitting neutral `WireMessage`s — and it is what lets the same numbers be priced by whichever model actually served each turn.
- A session spans routes. `routes()` returns every distinct provider/model in first-seen order precisely so a consumer cannot assume one price applied to the whole session; within a turn the **last** request header wins, because that is what the turn actually used after any re-route.
- The C11/P12 rules apply again, and mutation testing confirms they are load-bearing: a step that reported no usage increments `unreported_steps` rather than adding zero, and a turn where nothing reported stays `None`. Making either default to zero turns exactly those tests red. A session total is therefore a **lower bound** unless `is_complete()`, and "nothing to report" is complete while "reported nothing" is not.
- Attribute by turn index, never by adjacency. A log's turn events are not guaranteed contiguous, and an interleaved log must still attribute correctly — a test interleaves two turns and asserts both.
- Derive timing defensively. `duration_ms` uses `checked_sub`, so a clock that went backwards yields **no** duration rather than a wrapped one, and an unsettled turn (process died mid-turn) has none at all while its reported work still counts.

## 152. A rollback you do not verify is a rollback you are trusting

- K09 activates each plugin against a private `ContextFingerprint` — services, exact inventory rows, recorded `AppliedPlugin`s, descriptors, scopes, pending-effect count and total listener count. On failure it unwinds, then **re-captures and compares**. Anything left over becomes `CoreError::BrokenActivation` naming the plugin, the residue and the original cause. Unwinding without re-checking would have made every one of the mutations below survive.
- Ordinary failures still return the original error unchanged (GOTCHAS #28), so existing workspace error assertions keep passing; only genuine residue is re-classified. Composition still aborts and shuts down LIFO — nothing was made lenient.
- **The mutation that found dead code, not a bug.** Rollback truncated `ctx.plugins`/descriptors, and deleting those three lines broke nothing: they are unreachable, because nothing can fail after `record_plugin` commits. Unpinned code that cannot be reached is worse than absent — it looks like protection. It was removed; the fingerprint still watches that dimension, so a future post-commit failure fails loud instead of silently half-recording.
- **The transaction covers only what the `Context` owns.** A plugin that mutates an *earlier* plugin's already-published state — pushing into a registry behind an `Arc`, writing files — is invisible to the fingerprint and is not rolled back. This is the same rule as GOTCHAS #80 for commands, and it generalises: a contribution into shared state you did not create must carry its own disposer.
- A02 made that gap reachable, because seams are *the* plugin extension point. `Waterfall::push_effect` closes it: the layer is removed by the plugin's own effect on rollback or shutdown, and the disposer holds a **weak** handle so unwinding after the chain is gone is a no-op rather than a resurrection. `push_shared` is now documented as chain-lifetime only — for the chain's own owner. The `EventBus` already drew this line as `on`/`on_effect`; the seam simply lacked its half.
- No generic sweep can catch the `push_shared` case, and it is worth being precise about why: a chain lives behind a **type-erased** service, so `shared_layer_count` is available to a chain's owner auditing its own seam but unreachable from the fingerprint. The guarantee is therefore "the `Context` is restored", not "the world is restored" — and the discipline is what covers the rest.
- A successful activation report says only "every plugin activated", which `ctx.plugins()` already says. Its information is entirely in the failure, so the composition root spends it there: the operator whose heycode will not start is told which plugin failed, at which stage, and how many later plugins never ran.

## 153. A reload composes before it retires, and a retired world dies when its last reader does

- K10's acceptance is "successful reload swaps once; failed reload keeps last good", and both clauses fall out of one ordering decision: compose the candidate **before** taking any lock and before retiring anything. Shut-down-then-recompose makes a failed reload fatal — the world you were serving is already gone when the replacement turns out not to compose. A mutation that retires the live world first turns two named tests red.
- Generations count worlds that **ran**, not reload attempts. Three failed reloads in a row leave you on generation 1, so "still on 1" is a true statement about what is serving requests rather than a gap someone has to explain. Consuming a number on failure turns `a_failed_reload_keeps_the_last_good_world` red.
- Readers hold `Arc<Context>`, so a swap never pulls a world out from under work in flight. The retired world is parked and disposed only when `Arc::get_mut` succeeds — **that success IS the proof** that nobody is reading. Disposing on a guess is the exact failure a generation model exists to prevent.
- One mutation could not be written at all: "dispose the retired world while readers remain" requires `&mut` from a shared `Arc`, which needs `unsafe`, which the workspace forbids. The error is unrepresentable rather than merely untested — worth noticing when a mutation refuses to compile, because that is a stronger result than a red test, not a gap in the suite. The representable version (drop the handle instead of parking it) does turn the reader test red.
- A world parked forever because a reader never let go is **visible** (`pending_disposal`) rather than silently leaked. Deferred disposal must never become skipped disposal: a context dropped without `shutdown` leaks every effect it owns.
- Like K09's report, the model has no production consumer yet — `heycode-cli` composes once and never reloads, and no tracker row depends on K10. Recorded rather than hidden.

## 154. Two gates, not one: opting in to content is not opting in to credentials

- Q08's acceptance is "captures metadata/outcome, never credentials/content unless opted", and the trap is reading that as one switch. It is two. Content is withheld by default and released only by an explicit `ContentPolicy::RecordOptedIn`; credentials are refused **always**, including from an opted-in run. A single `verbose` flag would have collapsed them, which is exactly how a response body that echoes an API key ends up in a public dashboard.
- `ScreenedText` cannot be constructed except by passing the screen, so "an artifact cannot carry a credential" is a property of the type rather than a rule a caller has to remember. `FailureClass` and `SkipReason` are closed sets for the same reason S15's `WireExposureFault` is: a provider's error message routinely echoes the request, headers included, so the class is recorded and the message stays in the runner's own logs.
- A refused body produces **no artifact at all** rather than an artifact with that field dropped. A partial record leaves a reader believing they saw everything, which is worse than an obvious absence.
- Under the default policy the body is dropped **without being screened** — the cheapest way not to leak text is never to look at it. A mutation that screens the dropped body turns `a_withholding_recorder_drops_a_leaky_body_without_failing` red, which is the test asserting that withholding is unconditional rather than a screen that happens to pass.
- The screen is shared with S15 (`screen_text_for_credentials`), not reimplemented. There is exactly one list of what a credential looks like, and a second copy that drifts is how a leak ships. Exposing it from `heycode-settings` was the point of the change, not a convenience.
- Ten mutations, each red on a named test, including one that only matched a whole-string credential rather than an embedded token — the embedded case is the realistic one, since credentials arrive inside JSON error bodies.

## 155. A first execution is not a fast execution, and a 5-second budget cannot survive a scanner

- Eight tests failed across `heycode-runtime-claude` and `heycode-cli` with `Unavailable` and `Elapsed`, and they had passed an hour earlier with no relevant code change between. The cause was not the code and not parallel load: **executing a newly written binary took 11-23 seconds on this machine, while re-executing the same file took 5ms.** A one-line `/bin/sh` script measured 0.00s user, 0% CPU, 23s wall — it was waiting to be allowed to run, not working.
- Measure before concluding. "Fails under load" and "fails focused too" both pointed at a regression; the decisive experiment was timing a trivial new executable directly, twice. Ruling my own changes out mattered too: `heycode-runtime-claude` does not depend on `heycode-settings`, and the `heycode-core` delta was one unreferenced new module — so there was no path from my edits to a process spawn.
- Fix the test, not the product: both suites now execute their program once, untimed, before anything measures it. A fixture that writes a fresh executable per test pays that cost per test, which is an artifact of the harness rather than a fact about heycode.
- **The production question this raises is real and is NOT fixed.** `VERSION_TIMEOUT` is 5 seconds, so a user who has just installed or updated `claude` on a machine that scans new binaries will be told "Claude Code runtime process is unavailable" about a perfectly good binary. Worse, a timeout is being classified as `Unavailable` when it is honestly `Unknown` — the same distinction the capability model keeps everywhere else. Recorded for an owner decision rather than changed unilaterally, because widening the budget also delays detection of a genuinely hung binary.

## 156. PKCE is a proof, not a ceremony — and the state machine is where OAuth clients actually break

- MCP04's acceptance names four states, and that is the right emphasis: the interesting bugs in an OAuth client are state bugs, not crypto bugs. Refreshing after logout, exchanging a code against the wrong verifier, treating an absent `expires_in` as "never expires" — each was a mutation, and each turned a named test red.
- **`plain` is not represented.** `CodeChallengeMethod` has one variant, so there is no negotiation path to take. A server offering only `plain`, *and a server advertising nothing at all*, are both refused: unknown is not supported, the same rule the capability model keeps everywhere. Two mutations (accept `plain`, accept an empty advertisement) prove it.
- The challenge must be S256 of the verifier and the exchange must send the **verifier**, never the challenge. A client that sends the challenge has implemented the ceremony of PKCE with none of the proof, and it will still authenticate successfully against a permissive server — which is exactly why it needs its own test rather than trust.
- **State is checked before the code is read.** A callback wrong on both counts must report `StateMismatch`, which is what proves the ordering; without that case, a mutation that reads the code first survives. A forged callback's planted code must never reach the exchange.
- RFC 6749 §6 lets a refresh response omit a new refresh token, and the old one stays valid. Dropping it silently downgrades the session to one-shot — the next refresh demands a browser for no reason. This is the kind of defect that never throws.
- **One credential record, not three.** The whole token set is stored as a single JSON record, so the stated expiry cannot go stale independently of the tokens it describes, and logout is one delete. "Delete one thing" is provably total; "delete three and hope none is orphaned" is not, and a forgotten refresh token after logout is a live credential the user believes they revoked.
- A corrupt or older-format record reads as **absent**, not as an error: it must send the user through a fresh authorization, never wedge a server behind a failure they cannot clear.
- Faults are a bare closed tag (asserted at one byte). A token-endpoint error body is attacker-influenced and routinely echoes the request, so a denial carries the class and nothing else — and a transport failure is `Unreachable`, never `Denied`, because "the network broke" and "the server refused you" lead to different next steps.
- Form values are percent-encoded outside the unreserved set. Over-escaping always decodes correctly; under-escaping silently corrupts a redirect URI, and the failure surfaces as an opaque server rejection much later.

## 157. Four dependency cycles were stranding eight rows, and they all had the same shape

- A cycle check over `TASKS.md` found **five** cycles. Four had open rows and would never have unblocked: `U12 ↔ MCP10`, `U13 ↔ PL06`, `U16 ↔ C13`, `U18 ↔ A05`. Eight rows, three of them P1, sat permanently in the dependency-blocked bucket where nothing would ever surface them as ready. Under a mandate to reach zero pending, that is not a bookkeeping nit — those rows are unreachable.
- **Every cycle was a UI panel row and its backing-operations row each naming the other.** The panel genuinely needs the operations; the operations row named the panel only because its own title said "TUI". So the fix is uniform: break the operations → panel edge, and point the operations row at the UI *substrate* (`U03`, already complete) instead of the panel that consumes it.
- The fifth cycle, `K12 ↔ U01`, is already complete on both sides — which is the corroborating evidence. Nobody could have satisfied that ordering; the two were simply built together. A cycle that got completed anyway is proof the edge was never a real ordering constraint.
- Edges changed, minimally and reversibly: `MCP10 MCP01,U12 → MCP01,U03`; `PL06 PL02,U13 → PL02,U03`; `C13 C11,U16 → C11` (a projection needs no UI at all); `A05 A03,U18 → A03,U03`. Every new dependency was already `[x]`, so all four became ready immediately, and each panel row now unblocks from its own operations row.
- Worth running the check on any tracker used to pick work. "Dependency-blocked" is only meaningful if the blockage can eventually clear; a cycle disguises "impossible" as "not yet". The scan also caught `Q19` naming a dependency that is not a row id (`All P0/P1/P2 beta tasks`) — deliberate prose, left alone, but the kind of thing worth seeing.

## 158. Parity you maintain by hand is parity that drifts

- MCP10 asks for CLI/TUI parity across seven operations. The mechanism is a closed `McpOperation` enum that both surfaces match **exhaustively**, plus a single `McpManagement` that implements each operation exactly once. Adding an eighth operation then fails to compile in every surface that has not handled it. Most public enums here are `#[non_exhaustive]` so a new variant is not breaking — this one is deliberately the opposite, because breaking is the feature.
- `McpHealth` keeps the usual `#[non_exhaustive]`, and its default arm falls to **unknown, never reachable**. The two enums in one file want opposite defaults, and the reason is the direction of harm: an unhandled *operation* must stop the build, while an unrecognized *health state* must degrade to the safe reading.
- Both `--command` and `--url` is an error, not a precedence rule. A server has exactly one transport, and guessing which the user meant is how a server silently talks to the wrong endpoint.
- **A management command must not compose the product world.** The first wiring did, and `heycode mcp list` failed with "no API key for `deepseek`" — for a user who has not configured a provider yet, which is exactly the user setting servers up. It now composes two plugins (settings-file, mcp-management). Verified end to end with no credential present.
- **The reverse direction is no longer empty: the main world now READS that namespace.** `McpPlugin::apply` in product mode adopts the session's configured `[mcp.servers]` rows into `McpManagement` and then merges `McpManagement::connectable()` into the connection set, so a server added by `heycode mcp add` takes effect in the next session and configuration wins a name collision. The earlier note that "nothing in the main world reads the namespace yet" is retired; do not re-derive that gap. What is still one-directional is the standalone CLI: `compose_management_world` never loads `Config`, so `heycode mcp list/remove/enable` still sees only added servers while the in-session `/mcp` panel sees both.
- That failure also validated K09 in production: the error read "plugin `llm` failed to activate at the apply stage; 19 later plugin(s) never ran". Naming the plugin, the stage, and the lost remainder turned an opaque wall into a diagnosis.
- **The administrator lock is enforced at the store, not trusted to callers.** A definition from the managed settings layer arrives marked, refuses every mutating operation, and — the part worth a dedicated test — is never copied down into the user layer on persist, where the user could then edit it and escape the lock. Removing that filter turns exactly one named test red.
- A test premise can be wrong in the *safe* direction: I asserted a malformed settings row would be skipped, and the schema rejected the whole section instead, naming the offender. That is the better behaviour and the codebase's law, so the test changed to pin it. The store's skip branch stays and is separately pinned by a case the schema allows but the model refuses (a control character in a target) — otherwise it would be unreachable decoration, the K09 lesson again.
- `cargo fmt --all` while a lane is editing reformats that lane's file. Use `cargo fmt -p` on your own crates when anyone else is working in the tree.

## 159. Delegation is only as good as the verification behind it

- Five lanes finished their crates and **none delivered a report**, even after two rounds of direct messages. Green gates arrived; reasoning did not. Everything was verified directly instead: gates re-run per crate, acceptance read off the test names (which state their contract here, so the suite doubles as an audit), and every factual claim checked against live documentation.
- **Check the citation the code actually gives, not the one you assume.** Looking for MiniMax's `sk-cp` key prefix, I fetched three plausible pages, found nothing, and briefly had a fabricated-constant suspicion. The crate cited two *different* pages, and both carry the string. The lane was right and my check was sloppy — read the citation before doubting it.
- Signals that a delegated crate was done honestly, worth looking for: it cites the page next to the constant; it encodes **no** model ids where a catalog should supply them; and it leaves a fact `None` when the vendor documents none. PMM01 refused to invent a pay-as-you-go key prefix because "an unverified prefix would reject valid keys, and third-party claims are not evidence" — unprompted, and exactly right.
- Read acceptance off the tests, not the titles. "Account/project/location health checked" is satisfied by three *distinct* project states — confirmed, unset, undetermined — with a test that an undetermined credential document leaves the project undetermined and not unset. A single boolean would have passed a title reading and failed the row.
- **The session scratchpad is shared across lanes.** Two independently chose `<scratchpad>/pristine/src` for mutation snapshots; one overwrote the other, and the loser's restore step copied the wrong crate's source over its own. Nothing failed loudly — the damage was only visible on reading the file. Lane snapshots must be lane-unique. The lane that lost work reported it clearly and rebuilt from documentation rather than memory, which is the right response and worth saying out loud.
- Two self-inflicted cross-lane faults on my side, both avoidable: adding a crate to the workspace with a manifest but no `src/lib.rs` broke every `cargo` invocation tree-wide for minutes, and `cargo fmt --all` reformatted a file a lane was actively editing. When anyone else is working in the tree, create manifest and lib together, and format with `cargo fmt -p`.

## 160. A redundant check is a check no test can distinguish

- S14's first cut decided "is this field managed?" twice: once from `SettingsSnapshot::managed_locks()` and once by looking in the managed layer itself. Both were correct, so **removing either one changed nothing** — the mutation survived, and the rule was effectively unpinned. `managed_locks` is exactly the managed section's leaf paths, so the two could never disagree. Keeping one (the authoritative one) makes the mutation fatal.
- The second survivor was a test that could not fail. The origin test put each path in exactly one layer, so reversing the precedence order changed no outcome. **Precedence only means something where layers overlap**: the fixed test puts one path in all three layers, one in two, one in one, and one in none, and asserts both the origin and the effective value.
- Two survivors out of nine in code I wrote while telling seven lanes that a surviving mutation is a defect in their tests. Mutation testing is not a formality you perform on other people's work.
- The design rule this row is built on: **a secret has nowhere to be.** `SettingsField::Secret` carries `configured: bool` and no value field, so no renderer, log or snapshot can leak one, and no future edit to a render path can reintroduce the bug. Redacting before rendering would have been a rule to remember; this is a rule the type system keeps.
- Two screens in order of authority: a wire-exposed namespace's `redacted_paths` (S15 already screened it, and it catches a field declared secret despite an innocuous name), then the shared `names_credential_material` recognizer for namespaces with no declaration. One list of what a credential looks like, shared with Q08.
- An unrenderable schema construct becomes a visible `Unrenderable` row rather than being dropped. A field the user cannot see is a field they cannot fix, and a form that silently omits it lies about what the namespace contains.
- A managed value renders read-only rather than editable-then-rejected. Discovering a lock by being denied is a poor experience; an edit silently overridden at the next resolve is worse.

## 161. A constructor invariant that deserialization can bypass is not an invariant

- The TEL02 lane, reviewing Q08 unprompted, noticed that `ScreenedText` was `#[serde(transparent)]`. My Q08 log claimed "there is no way to build one except through `ScreenedText::screen`, which is what makes 'an artifact cannot carry a credential' a property of the type". **That claim was false on the read path**, and a probe proved it in one line: `LiveArtifact::from_json` returned `Ok(Some("sk-proj-AAAA…"))`.
- Ten mutations had passed over that code and none touched deserialization, because every one of them mutated the *write* path. A validated newtype has two doors and mutation testing only walks through the one you are thinking about.
- The fix is `#[serde(into = "String", try_from = "String")]` routing construction through the screen (`transparent` and `try_from` are mutually exclusive, hence `into` for the write side). **Every validated newtype in this workspace that derives `Deserialize` needs this audit** — the invariant is only as strong as its weakest constructor, and `Deserialize` is a constructor.
- Worth saying plainly: a lane found this in my code, having been told to review a reference implementation. Delegation paid a dividend in the direction I was not expecting.
- **The follow-up sweep is the interesting half, because the rule is not universal.** Every other `#[serde(transparent)]` newtype in the workspace derives `Serialize` **only** — no read path, no hole. Exactly three types derive both with a validating constructor: `ScreenedText` (the bug), telemetry's `Label` (already correct), and `InboxMessageId`. The core id macro is fine because `from_raw` validates nothing, so nothing is claimed.
- `InboxMessageId` is a **deliberate exception, and applying the rule to it made things worse.** I added the `try_from` guard, and it broke a passing test: an invalid id in a log went from `OpenError::InvalidEvent` to `CorruptLine`, and the session-query path treats `CorruptLine` as a possibly-transient partial write and **retries three times**. A permanently invalid id would have become three futile attempts. A03 already validates every deserialized id at the inbox projection — the layer that has the context to classify the error correctly. The guard belongs where the diagnosis belongs; reverted, with the reasoning written next to the derive so nobody "fixes" it again.
- So the rule is: a validated newtype deriving `Deserialize` needs *a* re-validating guard, not necessarily one inside serde. Ask which layer can classify the failure, and put it there.

## 162. `Context::provide` already records the Service row — declaring it again fails composition

- My lane briefs said "declare every contribution in `Plugin::inventory()`". That is **wrong for services**: `Context::provide` records the exact `Service` row and attributes it automatically, so re-declaring it in `inventory()` is a duplicate row that fails at the Declaration stage. Two lanes (PLM01, TEL02) hit it, checked the shipped plugins, found they all leave `inventory()` empty for services, and told me rather than working around it.
- The correct rule: `provides()` declares the key; `inventory()` declares contributions the context does **not** record for you — settings namespaces, catalog sources, authorization flows, tools, commands. Both lanes pinned "exactly one row exists, owned by this plugin", which is the assertion that actually matters.

## 163. Mutation testing in a shared tree: three ways it lies to you

All three came from lanes running mutation harnesses concurrently in one workspace, and all three cost real hours.

- **A killed harness does not kill its `cargo` child.** The orphan finishes compiling the *mutated* source and writes the artifact *after* the restore, so the fingerprint outranks the restored file and the next run silently reuses a mutated binary. The symptom is a red baseline against provably correct source — `diff -r` clean and the suite failing at the same time, with failures that perfectly implicate real production code. Trusting that signal means "fixing" working code. Fix: stamp mtimes on every restore and mutation write so no artifact can look fresher than its source, and spawn cargo in its own process group so a kill reaps the child.
- **`pkill -f <name>.py` substring-matches every lane's harness.** One lane's cleanup killed another's mid-run. Renaming to `<lane>-mutate.py` does not help — `pkill -f 'mutate.py'` still matches. Lane-unique names must share no substring with another lane's, and kills should use a full path or an exact pid.
- **A mutation can be "caught" by hanging rather than failing.** One made a test helper loop forever, which is indistinguishable from a loaded machine. Bound every helper loop by the closed set it walks so the hang becomes a named failure.
- Related shared-tree hazard: `cargo clean -p <one-package>` removed 244,445 files and 16.6 GiB from the shared `target/`, forcing every lane into a cold rebuild. Clean an isolated `CARGO_TARGET_DIR`, never the shared one.

## 164. A hook is someone else's code on your critical path

- O08's acceptance names four things — pre/post lifecycle, timeout, trust, disposal — and three of them exist *because* a hook is arbitrary third-party code running at a moment heycode chose. The fourth is what makes them meaningful.
- **The phase decides whether a failure is a veto, not the exit code.** A non-zero exit from a `Pre` hook refuses the operation; the identical exit from a `Post` hook cannot, because the operation already happened. `HookPhase::can_refuse()` puts that in the type, and mutations in both directions turn named tests red.
- **A broken hook must not become an outage.** A timeout, an unlaunchable command or an untrusted workspace produces `Faulted`, which still *proceeds*; only a deliberate non-zero exit from a `Pre` hook refuses. A fault also does not stop later hooks, while a refusal does — a refused operation must not keep running the hooks meant to observe it.
- `Unknown` trust is not trusted. K12's rule is that project executable contributions stay inert until an affirmative decision exists, and the absence of a decision is not an affirmative one. A user-scoped hook is ungated, because the gate is about code that arrived with the checkout.
- There is deliberately **no non-effect registration**. A hook is executable code; one that outlives its owner is code running for a plugin that is gone.
- Classify by the typed exit, never by parsing an error string. My first version matched `error.to_string()` for "timeout" — `ProcessExit` already distinguishes `Exited`/`Signalled`/`TimedOut`/`InactivityTimedOut`, and a string match would have silently mis-classified the day someone reworded a message.
- **Two of my own tests were decoration, and mutation found both.** The cancellation test asserted only the outcome, so removing the pre-launch check survived — the shell refuses a cancelled token anyway and the answer looked identical. It now counts launches through an injected backend and asserts zero, with a companion test proving the counter counts. And the timeout test *hung* under its mutation instead of failing (u12's lesson, GOTCHAS #163): it now bounds its own wait with `tokio::time::timeout`, so a missing budget fails in 30s rather than running for ten minutes.

## 165. A dependency list can be wrong by understatement as well as by cycle

- PL03 declares contributions for skills, commands, agents, hooks, themes and providers, and its dependency list was `PL01` alone. But **no hook registry and no theme registry existed** — O08 builds the first and U20 owns the second — so "each contribution activates/disposes in real composition" was unsatisfiable for two of the six kinds, and the tracker showed the row as ready.
- This is the same class of defect as the four cycles (GOTCHAS #157) and the opposite direction: a cycle makes a reachable row look blocked, an understated dependency makes a blocked row look ready. Both are found by asking "does the thing this row needs actually exist yet?" rather than trusting the edge list.
- Corrected to `PL01,O08,U20`, which is a **less** reachable row than before — so the bar for making that edit is higher than for removing a cycle, and the justification has to be a concrete missing artifact rather than a feeling. Here it was two absent registries, verified by grep.

## 166. Provenance must name its own source, or the boundary lies

- Two MCP lanes independently hit the same wall: `UntrustedContentSource` had exactly one variant, `Web`, so the only boundary they could construct would have stamped MCP server content `UNTRUSTED WEB CONTENT`. Both refused, and both took a worse API — passing the boundary in as a parameter — rather than make a false claim about where content came from. That was the right call: a boundary whose label lies is worse than no boundary, because it tells the reader something specific and wrong.
- `UntrustedContentSource::Mcp` now exists, labelled `MCP SERVER`. MCP resource bodies, prompt messages and the handshake `instructions` are all authored by whoever runs the server, and the specification itself says instructions MAY be added to the system prompt — a prompt-injection surface with its own provenance.
- The shape both lanes converged on independently is worth copying: no `Display`, no `Deref`, no `AsRef<str>`, a raw accessor named `untrusted_text()` so the warning sits at every call site, a `Debug` that reports only a byte count, and exactly one model-visible projection that requires holding a core boundary. You cannot render server text to a model by accident.

## 167. "These two happen to agree" is not "these two cannot disagree"

- PMM02's whole row is that two different MiniMax list endpoints normalize to one vocabulary. A mutation made documented models take their display name from the *wire* instead of the maintained table — and every test still passed, because the fixture was MiniMax's own published example, where `display_name` already equals the id on every row. The test proved the *accidental* version of the property while the module doc claimed the *structural* one.
- The fix keeps the shared fixture faithful to the vendor's published example and adds one test that feeds a documented id back with a **different** wire name, asserting both dialects still agree. A property that holds by construction needs a fixture where the accidental version would fail.
- The same lane found a second test passing for the wrong reason: a cursor-repeat test scripted two identical pages, so the duplicate-id guard fired first and the cursor guard was never reached. The test's name was a lie. When two guards can catch one fixture, assert *which* one did.
- A third instance from another lane: an oversize test whose rows were all uncallable failed on "nothing callable" rather than the size cap, so removing the cap survived. All three are the same lesson — **when one failure has two possible causes, assert the message, not just the kind.**

## 168. Verify twice before doubting a lane

- A lane reported clippy clean; my check showed two errors. The difference was a build-directory lock — with a dozen lanes sharing one `target/`, a single `cargo clippy` run can pick up transient errors from a crate another lane is mid-rebuild on. The re-run was clean and the lane was right.
- The same contamination explains three test failures in an earlier full-workspace gate, all in a crate whose lane was actively editing it. A shared-tree gate is a snapshot of a moving tree; a failure in a crate someone else owns is a question, not a finding.

## 169. "Unknown provider" was false, and the false half was the actionable half

- `heycode` rejected `provider = "anthropic"` with *"unknown provider `anthropic` — available: deepseek, openrouter"*, while shipping an Anthropic profile, authorization flow, model catalog and token counter. The gate was **correct** — only DeepSeek and OpenRouter have a `heycode_llm::Provider`, so no turn can run — but the message sent a user looking for a typo in a name that is not misspelled.
- Several providers now ship a profile, an auth flow and a catalog with no inference route. That is a real, legitimate intermediate state, and it needs its own sentence: *"provider `anthropic` has a profile and catalog but no inference route yet"*. Two lists, asserted disjoint, so a provider cannot be described as configured-only while actually being selectable.
- The general shape: when a capability arrives in layers, the error at the boundary has to name **which layer is missing**, or it reports the wrong problem with complete confidence. This is the same failure as a health check that says "unavailable" when it means "not checked".

## 170. Decide a conditional mount before composition, not inside it

- The AWS catalogs hard-fail `apply()` without a resolved region — deliberately, so an unconfigured region is loud rather than silently pointed at the wrong account. That makes them unmountable in the unconditional default set, and the decision has to happen *before* the plugin list exists.
- The trap, named by the lane that built the region resolver: the instinct is to ask `AwsAuthService`, which **does not exist until after composition** — too late to decide whether to add the plugin. Region resolution is a pure function over the process environment and answers at factory-table time.
- `default_profile` filters `BUILTIN_PLUGIN_ORDER` by which factories are *registered*, so the clean gate is simply not registering the factory. No new mechanism.
- Only `Resolved` mounts. `Unresolved`, `Malformed` and `Undetermined` all decline, so the tri-state that governs the status report governs the mount too — a malformed `AWS_REGION` yields a provider that is not offered, rather than a failed startup.
- The decision is a pure function taking an injectable host so it can be tested; the process environment is not injectable without `unsafe`, which this workspace forbids. The untested remainder is the single line that passes the production host.

## 171. A continuation token must return in the part that carried it

- Gemini requires a returned `thoughtSignature` "in the exact part where it was received", and validates the first `functionCall` part **in each step** of the current turn — a per-step property across a whole turn, where the turn begins at the most recent user message with standard content, explicitly *not* a `functionResponse`. A distilled signature field would satisfy a test and still 400 in production. PGCP03 stores the verbatim model `Content` and replays it unchanged, which satisfies the per-step rule by construction rather than by arithmetic.
- **Parts are never merged.** Coalescing two streamed text parts moves a signature off the part that owned it, so a streamed turn replays as the parts that arrived.
- **Record the raw part at the candidate level, not inside a normalization branch.** P06's `text` branch parsed the signature and returned before using it, and the documented streaming shape for a turn with no function call puts the signature on an **empty text part** — so that branch was the one that mattered. The fix was not to patch it: recording above the branches covers every branch uniformly and cannot rot the way a second code path can.
- **A replayed model turn is a model turn.** It opens the same function-response obligation, seeds the same id→name map, and needs the same declarations as a neutral one. The lossless path must never be the less-validated one — a real gap, found when the first replay test failed with "function response has no immediately preceding function call".

## 172. A comment can be unpinned protection too

- A mutation survived because a comment claimed an ordering was protective — "recording after validation is what stops a half turn being published" — when the parser's terminal flag already guaranteed it, so the ordering was unobservable. The lane kept the defensive ordering, **rewrote the comment to say what actually guarantees the property**, and recorded that mutation testing proved the line is not load-bearing.
- GOTCHAS #152 says unpinned code that looks like protection is worse than absent. This is the same failure one level up: a comment asserting a guarantee the code does not provide will be believed by the next reader and defended in review. When a mutation survives, check whether the surviving thing is a claim rather than a mechanism.
- The companion survivor in the same run: a single-chunk fixture made "publishes nothing on failure" unobservable, because the response failed before any valid part had been recorded — nothing could have leaked either way. **A property that needs two chunks to be observable needs two chunks.** Fifth instance today of a test passing for the wrong reason.

## 173. A hash you computed from the bytes you were handed proves only self-consistency

- PL02 resists substitution for an id/version it has **already committed**: the ref is no-clobber and `resolve` rehashes the object against it. It cannot resist substitution on a **first** install, because a never-seen id/version accepts whatever bytes arrive and the only digest in the system is the one the cache just computed from those same bytes. *Whoever writes the cache writes both halves of the comparison.*
- PL05 therefore does not re-implement a hash check. It supplies a **second, independent authority** — a publisher digest the operator configured, which the cache does not own — and re-applies it. The distinction is directly testable, and the test that proves it is the model of the genre: `bytes_that_miss_the_pinned_digest_are_refused_although_the_cache_itself_accepts_them` asserts PL02's `resolve` **succeeds** on the substituted bytes in the same test where PL05 refuses them. Two layers proven genuinely different rather than assumed to be.
- **Pin by what was requested, never by what arrived.** Reading the id and version off the arrived package and looking *that* up admits any version the marketplace happens to offer — a silent upgrade wearing a verification's clothes.
- **Origin requires host-held evidence.** A package's own `[source]` block is retained as a *claim* and never feeds `origin()`; `Unknown` is terminal and no constructor promotes it. The test installs a package claiming `kind = "marketplace"` and asserts it lands `Unknown` with the claim still reportable beside it.
- `SignatureState` has `Absent` and `Present` and deliberately **no `Valid`**. A digest that cannot be verified is carried, not enforced — the same rule applied to the manifest's `source.checksum` (it addresses an upstream artifact, not the canonical tree, so equating them would reject valid packages) and to git revisions (a `Revision` pin was dropped rather than recorded beside a verifiable one).
- Honest reach, stated in the crate: this makes a *pinned* marketplace substitution-resistant against a hostile catalog and a hostile package server. It does not authenticate a publisher, so a marketplace that can rewrite both its catalog and the digest an operator pinned is still outside what it can catch.

## 174. When a mutation survives, decide which report is correct before deleting anything

- PL05's silent-upgrade mutation survived because the comparison ran *after* the lookup, so changing the lookup key changed no outcome in any existing test. The reflex is to delete the now-apparently-redundant check (#160). The lane instead found the distinguishing case — a pin the catalog never offered, while the *installed* identity is offered — and then asked which answer is right.
- The answer: with no row for the requested version there is **no digest to verify against**, so the honest failure is "this pin cannot be resolved", not "substitution detected". Reporting a substitution would announce an attack the crate has no evidence for — the same lie as `SignatureState::Valid`. A new test pins that distinction and kills the mutation.
- Redundancy (#160) and under-specification look identical from inside a surviving mutation. The difference is whether the two paths would ever *disagree*; if they would, the survivor is a missing test, not a redundant check.
- A second survivor in the same run was an **equivalent mutation hiding an overclaiming assertion**: the test's message said "no oversize input is ever hashed", which is unobservable from outside and so could never fail. Rewritten to the observable claim — an oversize document is refused *on size, not on digest* — with the mutation moved to make the error class flip.
- And a third finding that mutation testing structurally could not reach: `PackageOrigin` was an internally-tagged enum with a newtype variant wrapping a transparent string, which **serde fails on at runtime**, and nothing in the suite serialized it. Provenance that cannot be rendered is not reportable, which is half of what the row promises. Found by reasoning about the serialization boundary — the same blind spot as #161, on the write path this time.

## 175. The owner is not exempt from the rule the owner enforces

- Every lane brief in this session says: read the tracker row verbatim, quote it back, and satisfy what it says rather than what it sounds like. Writing a brief today I quoted a Q14 that does not exist — wrong dependency (`Q13` instead of the complete `B03`), and an acceptance ("verified on all three OSes") belonging to a different row. I wrote it from assumption without opening `TASKS.md`.
- The failure mode is specific and worth naming: a row's *title* is memorable and its **dependency and acceptance columns are not**, so recall reconstructs the title accurately and invents the rest with equal confidence. That is why the rule is "quote it", not "know it".
- It also nearly caused the exact harm I corrected in another row the same afternoon: the fabricated acceptance would have had one row absorbing Q15's release channels and Q16's cross-platform matrix. Assumption expands scope; the file constrains it.
- Correct it in the open and say which parts of the earlier instruction to discard. A lane that has already started on a bad brief needs to know *which* half was wrong, not just that something was.
- **The lane caught it before the correction arrived**, by reading the file and checking its mtime to rule out a stale read — then refusing to pick between the two versions, because the tracker is the owner's to rule on. That is the right response to a contradictory instruction: neither obey it nor silently substitute your own reading.
- One trap in the aftermath: the lane found genuine supporting evidence for part of the invented acceptance (there is no three-OS CI, so "verified on all three OSes" would indeed be unsatisfiable) and proposed adding that dependency. Evidence that a fabrication *would* have been blocked is not evidence the fabrication was real. Understated dependencies get corrected; invented ones do not get ratified because someone found a reason they might have been true.

## 176. A stamped mtime must outrun the build, not the previous stamp

- GOTCHAS #163 says a restore-based mutation harness must stamp mtimes so a stale artifact cannot outrank restored source. A lane did exactly that — `stamp += 1` per write, starting a few seconds in the future — and still got **nine false survivors out of thirty-three**, each reporting the *previous* mutation's failing test. Each `cargo test` burns 10–40 seconds of wall clock, so cargo's own artifact stamp (written at "now") overtakes a source stamp that only advances one second per write. Cargo then silently reuses the previous binary.
- The fix is `max(previous + 1, now + 3600)`: the stamp must beat the clock, not merely the last stamp.
- **The only reason the nine were visible is that the harness required the *named* test to be FAILED rather than accepting a non-zero exit.** An exit-code check would have reported 33/33 killed and been entirely wrong — a mutation table that looks perfect and proves nothing. This is #167 ("when one failure has two causes, assert the message") applied to the tooling that checks the tests.
- A second hazard from the same run: a restoring harness holds a snapshot taken at preflight, so an edit made to a file mid-run is silently reverted at the next restore. Freeze the files under mutation, or diff afterwards.

## 177. Citing the source beats deferring to the instruction

- I told a lane that `NO_COLOR` set to *anything, including empty*, suppresses colour, and justified it as "the published convention". The published convention says the opposite: colour is suppressed *"when present and **not an empty string** (regardless of its value)"*. The lane quoted the text, corroborated it independently against a CPython fix titled "Fix `FORCE_COLOR` and `NO_COLOR` when empty strings", implemented the published rule, and put it in one function so overruling it is a one-line change.
- That is the right handling: when an instruction and its own stated justification disagree, follow the justification and say so. My constraint was not a policy choice I was entitled to make — it was a factual claim about a published standard, and it was wrong.
- The structural half matters as much as the correction. Putting a contested rule in a single named function with its citation next to it makes the disagreement cheap to resolve either way, instead of scattering the assumption through a detector.

## 178. A gateway-provided capability and a model-intrinsic one do not share an inference rule

- #121 licenses OpenRouter's catalog to mark `native_web` Supported for a model whose upstream lacks native search, because OpenRouter documents fallback search *for any model*: the gateway supplies the capability, so the claim is true at the layer that makes it. PGCP04 asked whether the same reasoning lets the Google catalog mark `image_input` Supported for a Gemini model whose docs do not state it.
- It does not, and the difference is not about confidence — it is about who provides the capability. Web search through OpenRouter is **gateway-provided**: the route can honour it regardless of the model. Image input is **model-intrinsic**: no gateway can add it to a model that lacks it. An inference rule that transfers between those two is inferring from the wrong noun.
- The decision was to leave `image_input` **Unknown**, which is the tri-state working as designed rather than a gap in the row. Unknown is what a catalog says when the vendor did not; guessing Supported to make a table look complete is how the state exists to be avoided.
- Test for the general case: ask *which layer would have to be wrong* for the optimistic mark to be false. If the answer is "the gateway, which documents otherwise", the mark is defensible. If it is "the model, which cannot be changed by the layer making the claim", it is not.

## 179. "The protocol supports it" and "a composed world can use it" are different claims

- Phrasing owed to the MCP09 lane. A row can satisfy the first and fail the second, and a tracker that only checks the first will mark it done. MCP09's prompt catalog parses, validates, and binds arguments, and refuses a missing required argument before a request exists — the protocol layer is complete and honest. Nothing outside the connection effect can reach `catalog.prompts()`, because no service key exposes it.
- Both statements are true at once: the row's acceptance is met, and the product cannot yet use the result. Record that as the gap between the row and the product rather than resolving it by moving the mark in either direction — MCP07's supervisor sat mounted-but-never-run through the same gap until MCP09 wired it.
- The general shape: a layer's correctness is not evidence of its reachability. When accepting a row, ask separately what proves the behaviour right and what proves it *reachable*, and if only the first exists, say which.

## 180. Repurpose a test whose premise your change invalidated; do not delete it

- Flipping `McpListingSupport::CURRENT.resources`/`.prompts` to `true` broke `resources_and_prompts_report_unsupported_instead_of_an_empty_list`, correctly: it pinned the pre-MCP08/09 truth that those sections report themselves unsupported rather than empty.
- Deleting it would have discarded a real invariant along with a stale premise. The invariant was never "these sections are unsupported" — it was **"an unasked zero and an answered zero must not render the same"**. That survives the flip; only which states embody it moved. The test now pins that a walked-to-zero family renders `0` while a never-advertised one renders `NotAdvertised` and still never `0`.
- The `Unsupported` arm itself is now unreachable from `CURRENT`, so a second test constructs the unlanded support set directly and keeps the arm exercised. Unreachable-today is not dead: it is where the next listing family (completions, roots) sits between "the section exists" and "the code lists it".
- Before deleting a failing test, separate the claim from the constants it was written against. Two mutations confirmed the replacements bite: reverting the flip failed two tests, and making `NotAdvertised` render `0 <noun>` failed the distinction test.

## 181. Honesty can cost robustness, and that trade needs an owner, not a default

- MCP09 made a required server that advertises a listing heycode cannot walk **fail composition**, where previously only a failed *tool* walk did. Approved deliberately: `McpContributionCounts` is documented complete and cannot express "unknown", so the alternatives were failing loudly or publishing the exact false statement — `0 resources` for a server that has some — that this stretch of work existed to remove.
- The mitigations are what make it acceptable, and they are structural rather than hopeful: retention means an *established* connection degrades rather than misreports, so only the initial connection fails — at the moment a user is best placed to fix it.
- The rejected alternative is worth recording. Widening the counts to `Option` would buy robustness against a flaky `resources/list` by reintroducing an unknown into a field documented as complete, plus a schema bump and a `heycode-tui` change. That unknown is how `0 resources` reached the panel in the first place.
- A behaviour change that trades a user-visible failure for a correctness property is an owner decision. A lane surfacing it and asking rather than choosing quietly is the behaviour to keep.

## 182. "The gate exists" is not "the gate passes"

- I wrote the Q07 cross-platform workflow and marked the row `[x]`. The row's acceptance reads **"Deterministic gates pass all three"**, and at that moment they did not: `heycode-extensions` does not compile for Windows (`no field `path` on type `FileLock``, `cache_store.rs:667`), and the workflow has never executed on any runner. I corrected the mark to `[~]` within a minute, but the mistake is the interesting part, not the correction.
- It is the same overclaim I had rejected from lanes twice the same day, made by the person enforcing the rule — see #175. The mechanism is worth naming: I had just done a genuinely thorough piece of work (cross-checked two targets, found a real defect, documented every gap in the file header), and the *quality of the work* is what made the mark feel earned. Effort is not evidence. The acceptance criterion is.
- Delivering the artifact honestly and marking the row honestly are separate acts, and the second is where the tracker's value lives. A workflow whose header documents in detail that it has never run, sitting under a row marked complete, would have been a document arguing with itself.
- The general check before any `[x]`: read the acceptance clause aloud and ask what would have to be **observed** for it to be true. "Passes on three platforms" is observed by three green runs, not by a file that would produce them.

## 183. A cross-check finds what only the target can tell you — and the toolchain gap is part of the finding

- `cargo check --target x86_64-pc-windows-gnu` on the development host found a real Windows build break in `heycode-extensions` that months of macOS development had not: a let-chain read `lock.path` without a cfg gate, and `FileLock` is `{ file, path, _process_lock }` under `#[cfg(unix)]` but a unit struct under `#[cfg(not(unix))]`. The branch **cannot execute** off-unix — `try_acquire_existing_file_lock` returns `Err(UnsupportedSecurity)` before `lock` binds — so it is a pure type-check failure in dead-at-runtime code, which is exactly the class no amount of running the tests on one platform will reveal.
- The obvious fix is the wrong one. Giving the unit struct a dummy `path` would make it compile *and* make `remove_if_owned` reachable on Windows, converting a deliberate fail-closed design into one that walks further into a path whose inode/nlink/flock guarantees do not hold there. When a cfg boundary breaks, fix the boundary, not the type.
- Equally important: the cross-check could not cover everything, and the gaps are results too. `ring` (via rustls) needs a target C compiler that this host lacks, so `heycode-skills`, `heycode-runtime-claude`, `heycode-runtime-codex` and everything downstream of `heycode-http` are **BLOCKED, not passing** — recorded that way in the workflow header. A partial cross-check reported as a clean one is worth less than no cross-check, because it retires the question.
- `--target x86_64-pc-windows-gnu` is also not `-msvc`, which is what CI runs. `check` does not link, so the delta is small, but "small" is not "none" and the header says so.

## 184. An audit of your own completed work finds things, and most of what it finds is wrong

- A read-only sweep of all 157 rows marked `[x]` raised **10 suspected overclaims**; adversarial verification — each suspicion handed to a second reader told to *refute* it — killed **6 of the 10** and confirmed 4. The refutation stage was not ceremony: it is the difference between a usable finding list and a pile of false accusations against finished work.
- Every one of the six refutations came from the same class of error: **the auditor read a stricter or different acceptance than the row states.** CAT05 had a UI obligation read into an API row. N03's "native fallback equivalence" was read as a specific vendor's fallback rather than provider-native vs portable. E12's auditor **found a real defect and attached it to the wrong row**. CMD02's read "all five commands persist a selection" where the acceptance said "dialogs and persisted selection work".
- So the discipline that catches overclaim is the same one that produces false alarms: reading the acceptance literally. The fix is not to read it less literally but to make someone else re-read it adversarially before it becomes a finding. **Confirmed-by-one is a hypothesis; confirmed-after-refutation is a finding.**
- PGCP04 was suspected and refuted, and the refutation is the standard working: the fixture half is met, the live half was not executed, and the tree says so in three places, one of which names the row id. A disclosed gap is not an overclaim.

## 185. The four ways a completed row was actually wrong

The confirmed findings sorted into four distinct failure shapes, worth naming because each needs a different fix:

- **K07 — the acceptance names a consumer that does not exist.** "`--profile` and picker apply same layered profile", with no picker anywhere in the tree; `NamedProfileStore::list()`, the picker-shaped half of the API, has zero production callers. Parity was established *by construction* — the test's "picker" is the same `store.load()` call the CLI makes — so it cannot detect the drift between two consumers that GOTCHAS #65 exists to prevent. The gap was disclosed in three places and contradicted in three others; the contradictions were the defect, and they are now corrected.
- **K08 — the acceptance named an output the design deliberately excluded.** "JSON/human output names dependency **and activation** failures", but the composition doctor was zero-apply by design and `describe_activation_failure()` was wired into `compose_world()`, the normal run path, never into the doctor and never into JSON. A row cannot be complete against an acceptance its own design refuses. This finding is now closed by the separate isolated phase in #207; the zero-apply graph remains intact.
- **CAT06 — half an acceptance met at the wrong layer.** "Source and timestamp retained" — `ModelPricing` is a bare map with no source field and no capture instant. Retention exists at the enclosing catalog generation, which is CAT02/CAT04's contract, not this row's. Meeting a clause somewhere in the system is not meeting it here.
- **C11 — the type exists and nothing ever fills it.** Every `TokenEnvelope` in the workspace is hand-assembled inside a test; no function takes a request, draft or session log and produces one, and the `ProviderState` contributor is never constructed anywhere at all. The row's acceptance ("prompt/tools/state/attachments included") describes a measurement that never happens outside its own tests. **A test that constructs the input it then measures proves the type compiles, not that the system uses it.**

## 186. Delegated work survives as files, not as agents

- A session restart ended 21 in-flight implementation lanes at once. Everything they had written to `crates/` was still on disk; everything they *knew* — which mutations they had run, what they had verified, which half of their row was finished, what they were about to report — was gone.
- That asymmetry is the thing to design around. A lane's value is not only its diff: it is the diff **plus the report that says what the diff proves**. Losing the second half turns finished work into unverified work, because nobody can tell a complete edit from an abandoned one by looking at it. Twenty-four crates carried edits of unknown completeness.
- It is worse in a repo with no commits. With everything untracked, `git status` showed 11 top-level paths and no diff baseline, so there was no way to separate this stretch's edits from the previous twelve hours'. **A commit is a checkpoint that survives process death**; without one, file mtimes are the only forensics left.
- Practical consequences adopted: prefer lanes that land small and report often over lanes that work for an hour and report once; treat an unreported lane's work as a *candidate* requiring independent verification, never as delivered; and when a stretch of delegated work completes, get it committed rather than leaving it resident only in a working tree.

## 187. A guard that understands one spelling of an address understands none of them

- QSEC03's adversarial matrix found that `ip_is_private` used `to_ipv4_mapped`, which recognises **only** `::ffff:a.b.c.d`. Three other standard ways of writing an IPv4 destination inside an IPv6 literal walked past every IPv4 rule, loopback included: `::7f00:1` (IPv4-compatible, deprecated but still parses and still routes), `64:ff9b::7f00:1` (the NAT64 well-known prefix), and `2002:7f00:1::` (6to4). Eight special-purpose IPv4 ranges — broadcast, multicast, `240.0.0.0/4`, `0.0.0.0/8`, `192.0.0.0/24`, `198.18.0.0/15` — were reachable outright.
- The fix that matters is structural, not a longer list. `embedded_ipv4` now resolves whatever IPv4 destination an IPv6 address actually carries and the predicate recurses into the IPv4 rules, so **every IPv4 rule applies through every encoding automatically**. Adding a range later cannot leave it reachable through a spelling nobody re-checked. Enumerating the eleven gaps would have fixed today's list and rebuilt the same trap.
- Over-blocking is the symmetric failure and is tested for: `ranges_that_look_private_but_are_not_stay_reachable` pins the neighbour of every boundary — 172.15 and 172.32, 100.63 and 100.128, 169.253 and 169.255 — because a guard that denies too much hides real denials.
- **The lane's reporting shape is worth copying.** It did not fix the hole; it wrote a *tripwire* test asserting the gaps were reachable **today**, designed to fail the moment any range was closed, paired with an `#[ignore]`d test stating the requirement. The finding could not drift from the code, and could not be quietly forgotten. When the fix landed, the tripwire had nothing left to record and was retired while the requirement was promoted to a gate (#180).

## 188. Do not derive `Debug` to satisfy a test

- `heycode-http`'s test failed to compile because `unwrap_err()` needs `Debug` on the `Ok` type, and `WebSocketRequest` has none — deliberately, with a doc comment saying its headers and opening frames may carry authorization values. The one-character fix (`#[derive(Debug)]`) would have silently converted a considered security decision into a leak path, to make a test compile.
- `.err().expect(..)` needs no `Debug` on the `Ok` type and was the whole fix. **The test bends, not the type.** When a type's absence of a trait is load-bearing, the compile error is the design working; read the doc comment before satisfying the compiler.
- Same session, same file: a test asserted `wss://provider.test:443/socket` round-trips byte-identically, which fails because `Url` normalises a scheme's default port away. That expectation pinned URL normalisation rather than the guard. Corrected to state the normalised form per case, plus a non-default port that must survive — the property actually worth holding.

## 189. I deleted ten security tests with an unanchored slice, and the transcript got them back

- Converting the QSEC03 tripwires, I edited by slicing between two markers: `s[s.index(start) : s.index(end)]`. `s.index` finds the **first** occurrence from the beginning of the file, and my `end` marker occurred *before* my `start` marker. Python happily returned a slice, `str.replace` happily applied it, `cargo fmt` happily formatted the result, and the tests happily passed — because the ten tests that would have failed were the ones I had just deleted. `ssrf_matrix.rs` went from 769 lines to 494 and from 21 tests to 10.
- **A green suite after an edit proves nothing if the edit could have removed tests.** The only reason I caught it was noticing the lib count drop from 24 to 14 in passing. Check the test *count*, not just the result, after any structural edit.
- The rule I already knew and violated: never slice between computed indices. Remove a function by locating its declaration, walking back to the blank line above its doc comment, and forward to the closing brace at its own indent — then assert the span is the size you expect before deleting. That helper worked first time on the retry.
- **Recovery came from the subagent transcript**, not from git — the repo has no commits. `~/.claude/projects/<project>/subagents/agent-*.jsonl` holds every tool call a lane made, including the full file contents it wrote.
- The first recovery attempt was still wrong, and the way it was wrong is the lesson: I searched the transcript for tool calls with a matching `file_path`, found a single `Write`, and restored it — but the lane had made three **further** edits through Bash heredocs, which carry no `file_path` field. Restoring the `Write` silently reverted the lane's own bug fixes, and two tests failed with errors that looked like my guard changes had broken them. **Search a transcript by content, not by tool signature**, then replay the edits in order rather than restoring a single snapshot.

## 190. Fixing a guard breaks the tests that reached past it

- Three tests failed after the SSRF hardening, none of them defects in the fix.
- `admission_denies_any_private_answer_and_pins_one_public_resolution` built its case by redirecting `https://…` to `http://169.254.169.254/…`. That hop is now refused by the downgrade rule, one layer *earlier* than the test's subject. The test's claim — that admission refuses cloud metadata — is still worth holding, so the redirect stays on https and the admission layer does the denying. Rewriting the assertion instead would have moved the test's subject to whichever layer happened to fire first.
- The general shape: when a new guard makes a path unreachable, any test that *traversed* that path to reach a deeper layer now stops early. Ask what layer the test is actually about, and re-route it there — do not weaken the new guard, and do not let the test silently start asserting something else.

## 191. A read-only subtree was unenforceable because access filtered the candidates

- `LocalFileSystemBackend::target` picked a root by walking every configured root, skipping any that could not serve the operation (`if write && !root.access.permits_write() { continue }`), then choosing the most specific of what remained. Declaring the workspace read-write and `workspace/protected` read-only therefore did the opposite of what it says: a write to `protected/file.txt` skipped the read-only root and **matched the read-write parent instead**, and the write succeeded.
- Skipping a candidate does not deny the operation — it hands the match to a less specific rule. **Selection and permission are separate steps and must stay in that order**: choose the most specific root that contains the path, then ask whether *that* root permits the operation. Filtering first makes a nested grant unenforceable by construction, and the nesting is the only reason to declare it.
- The error shape survived the change: a path under only read-only roots still reports `ReadOnlyRoot`, and a path under none still reports `OutsideAllowedRoots`. The `any_root` flag that used to reconstruct that distinction after the fact became dead once the selection was honest.

## 192. `Path::components()` cannot see `.`, so a `CurDir` check is dead code

- The same boundary rejected traversal with `components().any(|c| matches!(c, ParentDir | CurDir))`. `Path::components` **normalises `.` away** — `/ws/./f` yields `RootDir, Normal("ws"), Normal("f")` — so the `CurDir` arm could never match and only `..` was ever caught. The check read as if it covered both.
- A `ResolvedPath` is canonical by construction, so one arriving with a dot segment was not produced by `resolve`, and the boundary is where that gets caught rather than assumed. The replacement splits raw segments on both separators, which is the only way to see what `components()` has already discarded.
- General rule: when a check delegates to a normalising API, confirm the thing you are checking for survives the normalisation. An arm that cannot fire is worse than a missing check, because it reads as coverage.

## 193. A generic error hid the actionable half of a root substitution

- Removing a workspace root and renaming a decoy into its place was correctly *refused* — but as `NotFound`, because every operation through the retained `Dir` handle fails with a bare ENOENT once the directory is unlinked. The caller learned "not found" about a path that plainly exists, and the substitution — the fact worth acting on — was exactly what the generic error hid.
- The identity check already existed, but only at commit, which the write never reached: it failed at the first `create_dir_all`. Verifying the root's identity *before* touching the retained handle attributes the failure, and the commit-time check stays because the root can be swapped while the temporary file is being written. **Two checks, two different questions**: "was this already substituted" and "was it substituted during the write".
- Identity, not path: a replaced root keeps its name and canonical path while becoming a different directory. Only the stamp taken when the service was built can tell them apart.

## 194. Two assertions that forbade the payload the test itself planted

- QSEC03's Seatbelt escape matrix planted a crafted root containing `"))(allow file-write*)…` and then asserted `!encoded.contains("\"))(allow")`. In Rust source that literal is `"))(allow`, and correctly escaped JSON — `\"))(allow` — *contains* it. The assertion was unsatisfiable: correct output failed it, and no incorrect output could pass. Restated positively, the property is that the quote appears backslash-escaped.
- The bwrap case had the same shape: `!argv.iter().any(|e| e.contains("file-write"))`, where the crafted root *is* the payload, so the two legitimate `--bind` elements carrying it tripped an assertion meant to catch splicing. The real property is narrower and stronger — wherever the payload appears it is a *whole* argv element, never a fragment of one bwrap built by concatenation.
- Both are test defects, not code defects; the sandbox escaped and passed the payload correctly throughout. When a test plants a payload and then forbids that payload's text, the assertion is about **where** the text may appear, not **whether** — say so, or it asserts nothing it can survive.

## 195. `turn/end` does not close a started step, and publishing the error first lies

- A07's first owner test failed with one record: `Step { turn: 1, step: 1 }` was `Interrupted`. The request-error owner appended `turn/end error` but never `step/end`, so C08 correctly reported that the handled failure left work open. Closing an outer scope does not retroactively close its inner scope.
- Failure stage determines the durable closure. `Prepare` and `Stream` occur after `step/start` and before `step/end`, so they append both `step/end` and `turn/end error`. `PreStep` has not opened a step, while `Invariant` is used only after the step settled; both append only the turn end. Encoding this at the one failure owner avoids duplicated guesses at every return site.
- A bare `?` after `step/start` bypasses that owner just as surely as a hand-written `return Err`. Request construction, request rebuild, inbox drain and ordered tool-batch errors now route through the stage-aware closure. Search the whole turn body whenever another fallible operation is added.
- `UiEvent::Error` used to publish before `turn/end` was durable. A synchronous listener could therefore observe and render failure while the log still described an active turn. Closure now commits first, then emits; a regression listener inspects the session at event time and requires `step/end` plus `turn/end` already present.
- Tool execution errors are outcomes, not turn aborts. They commit an error `tool/result`, every sibling call still receives exactly one result, and the next provider step proceeds. The same C08 oracle proves this normal failure path leaves no open record.

## 196. A provider can measure a request without being able to partition it honestly

- C11's first implementation had strong arithmetic and no producer. Every `TokenEnvelope` was hand-assembled in a test, `ProviderState` was never constructed anywhere, and no composed Agent held the counter registry. A type with exhaustive unit tests is not a product feature until a production boundary builds and publishes it.
- Strict and compatibility routes have different authoritative inputs. A strict adapter already owns the exact `ResolvedCall`, including lossless provider state; rebuilding from a neutral `ChatRequest` would drop the contributor the row explicitly names. Compatibility routes have no such state plane and measure their real `ChatRequest`. Two producers over one shared partitioning rule are the honest boundary.
- A provider count endpoint normally measures a whole request. Asking it to count System alone or Tools alone requires a fabricated transcript, and summing several provider-exact subset requests can double-count framing while still being labelled Exact. C11 therefore uses the provider-selected registry only for real transcript Messages. System, tool schemas and lossless state use the named local estimate, so any non-zero one weakens the total to Estimated.
- Media must be removed from the countable message clone and represented independently as `Uncounted(Unmeasurable)`. Letting the heuristic reject the whole message would lose its text; leaving media on a provider counter that cannot encode it would under-count while claiming exactness.
- Counter fallback is evidence. `TokenCountOutcome::refused` cannot disappear when converted into an envelope, so `EnvelopeEntry` retains the better-ranked refusals beside the winning count. Anthropic's exact counter now refuses tool results, assistant tool calls, images and documents instead of silently serializing only their visible text.
- Agent and native subagents now declare/inject `token-counters`; schema v20 inserts it before the first historical exact-profile Consumer. Making the lookup optional would turn a composed-world guarantee back into a hidden fallback. Measurement owns a caller-child token, cancellation cancels and awaits the counter, and only a complete envelope is published.

## 197. A durable store with no factory is a library, and two independent caps need two independent tests

- TEL05 arrived with a correct `HealthHistoryStore`, an effect-owned plugin and nineteen strong tests, but no composition-root factory, no default row and no built-in service key. `/health` could not exist in a real product world. The first production composition test failed on the missing plugin, which is the observable difference between “the store works” and “a user can use it”.
- The store has two independent bounds: entry count and exact serialized bytes. Ordinary entries make the count bind first; maximal entries make the byte budget bind first. A test for only one lets the other become decorative. Every **load** re-applies both too, so a different build cannot hand a Consumer an unbounded history.
- Failure retention is a preference, never an exemption. The most recent unhealthy runs are protected from a burst of healthy entries, but when every entry is unhealthy the oldest still leaves at the hard cap. Otherwise an attacker could keep the file unbounded simply by writing failures.
- A non-newline-terminated final row is a torn append and is reported separately; a newline-terminated malformed row is corruption, costs one entry, and never hides the valid tail after it. The next record rewrites only the valid bounded prefix while returning the damage count to the caller, so repair is visible rather than silent.
- File order is authoritative. Sorting by timestamp would reorder evidence when the host clock moves backward. Newer-schema or foreign-header files are refused and left byte-exact, while missing/empty files are adoptable. Labels pass the shared credential screen both when constructed and when deserialized, so a hand-edited history cannot reopen the write-side leak path.
- Product wiring uses a composition-root-supplied path beside the isolated settings home, registers the service/command as normal effects, and adds no schema migration because no historical plugin acquires a new injection. Default worlds gain the optional capability; intentional exact profiles remain exact.

## 198. A shared resolver is not a shared feature until both production consumers exist

- K07 originally had one strict `NamedProfileStore`, a CLI caller, and a test that called the same store twice while naming one call “picker”. That proved the resolver, not the acceptance: no picker existed and `list()` had no production caller.
- The missing boundary is now an effect-owned `profiles` service. CLI startup and the TUI command/modal both cross that service/store contract, while the real composition test observes service → `/profile` → typed selection and the real-binary test observes the selected layer changing the world. Two consumers can now drift, so the tests can detect drift.
- A composition-changing command must not silently kill active work. `/profile` is queued; after selection the TUI returns a typed outcome, settles every task, restores the terminal, and only then lets the CLI replace the prior `--profile` pair and rebuild from all original arguments. Browsing configuration is not permission to cancel a turn.
- Historical exact profiles are the compatibility cost of adding an injection. Schema v21 inserts only `profiles` and `commands` before an existing TUI Consumer; profiles without TUI remain untouched.

## 199. Dropping a Context allocation does not execute its effects

- `Context::shutdown()` owns LIFO disposal. Rust field drop alone only drops the vector of disposer closures; it does not call them. A generation registry that releases its final `Arc<Context>` without an explicit shutdown leaks every registration/process/listener semantically even though memory is reclaimed.
- `GenerationContext` is therefore the terminal owner and calls shutdown in `Drop`. A retained reader may outlive both a swap and the registry, and that is safe: its exact world remains open until the reader releases the final strong reference, then shuts down once.
- The Q06 property model includes registry destruction, not only reload calls. It derives every live/parked/disposed generation and seam layer from generated operations, then requires every activated world—including rejected candidates—to have unwound after the last reader. A zero-operation sequence kills a mutation that removes terminal shutdown.

## 200. Route selection has two commit points, and backend publication is second

- Provider/model/runtime settings are durable authority; an app-server backend is live publication. Publishing the backend first and then losing the Settings CAS leaves the next turn running a route the durable record denies. X02 builds the candidate, commits through `RoutingService`, and swaps the backend only after success.
- One operation gate covers active turns and selection. Provider/model/runtime changes fail with conflict while a turn owns the read side; an opened or differently linked runtime session cannot be rebound. A no-op selection of the current runtime returns the current durable route without inventing work.
- `workspace/select` is authority-bearing input: it must be absolute, existing, canonical, a directory and contained by the composed root after symlink resolution. The native Agent's tool roots are fixed at composition, so native relocation is honestly Unsupported; delegated runtimes receive the selected canonical path on start/resume.
- Capability flags follow reachability. A base app-server advertises runtime/workspace controls false. The effect-owned control generation flips them true only while the corresponding methods are installed and dispatchable; protocol vocabulary alone is not product support.

## 201. A sandbox test inside another sandbox is evidence about both layers

- QSEC03's ordinary Seatbelt runtime cells failed in a separate Codex task after a profile encoder change, while all shape tests passed. The tempting diagnosis was a macOS profile-parser regression. It was wrong: that task itself lacked authority for nested `sandbox-exec` launches.
- Diagnose the layer before editing. The root host directly parsed benign, quote-escaped and backslash-escaped profiles successfully; the exact failed sandbox library target then passed unchanged from an unrestricted root environment. The encoder was correct and no code change was warranted.
- Confinement tests need an execution environment capable of creating the confinement they test. A denied nested sandbox is not a product failure, and an escalated rerun is not the same check: record the outer policy as part of the evidence. Conversely, a passing shape test cannot replace native enforcement; both parser/runtime and structural assertions remain.

## 202. A descendant that writes before cancellation did not escape cancellation

- The Agent process-tree test launched `(sleep 0.65; write-marker) &`, waited for a readiness file, then cancelled. In a seven-package parallel gate the test task did not regain CPU before 650 ms, so the child wrote **before** cancellation and the assertion called that an escape.
- Wall-clock delay is not synchronization. The replacement child waits for an explicit release file. The test cancels and awaits the turn first, then creates that file; only a descendant that survived the process-group kill can observe it and write the marker. Heavy scheduling can delay the observation but cannot invert cause and effect.
- A timeout remains appropriate for “the operation must settle,” but not for ordering two actors. Use a channel, pipe, file, barrier or other explicit handshake for the ordering property and reserve elapsed time for the outer liveness bound.

## 203. Rich results need a pending plane and a durable plane

- MCP12's parser already preserved image/link/structured blocks, but the registered Tool converted them to display text. That made the protocol type unreachable from Agent/session: image bytes vanished behind `[image …]` and a unit test of the parser could not detect product flattening.
- The shared Tool boundary now returns ordinary JSON plus an optional bounded **pending** rich result. Pending media is decoded but not serializable/loggable. Agent's model-order commit cursor admits each body through ATT01, then builds a **durable** result containing only immutable attachment metadata. Raw bytes never enter UI JSON, session lines or Debug.
- `tool/rich-result` is a new v2-only closed event kind rather than an optional field on `tool/result`. Older readers reject it instead of silently ignoring the data that distinguishes rich from plain. Repair treats both kinds as call settlement; projection/provider/TUI/runtime derive from the durable object.
- Presence is data. `structuredContent: null` is not absence, so the durable schema uses `Absent | Present(Value)` rather than `Option<Value>`, whose serde representation collapses JSON null into None. Server `isError` is retained beside the rich object; errors do not fall back to a string-only ToolError.
- Commit order is bytes → `attachment/added` → `tool/rich-result` → live UI. A crash in between leaves admitted bytes and an honestly open call, never a rich result pointing at bytes that did not commit.

## 204. “Telemetry off by default” is a dependency property, not a boolean

- Putting an `enabled=false` flag beside an HTTP exporter still gives the default telemetry crate the code and dependencies to emit. TEL02's stronger guarantee is structural: `heycode-telemetry` cannot name HTTP or credentials. TEL03 therefore lives in separate `heycode-telemetry-otlp`, which cannot weaken the local-off source/manifest audit.
- Availability and selection are separate. The CLI factory table knows `telemetry-otlp-http`, but `BUILTIN_PLUGIN_ORDER` contains only `telemetry-local-off`. A named profile must disable one and enable the other; selecting both fails the duplicate `telemetry` service key instead of layering egress onto the default.
- Export authentication is a reference resolved per batch, not a secret copied into settings or retained by the transport. Rotation reaches the next request. Settings expose only endpoint/resource labels/header/reference/kind/scheme and are restart-applied, because changing an egress destination under a live exporter would split one process across authorities.
- A crate-local exporter test was not product reachability. TEL03 became complete only after a real production factory/profile composition proved the selected service owner, settings namespace and `/plugins verbose` inventory while recording no network traffic.

## 205. A reasoning capability does not evidence every reasoning control

- The recovered Z.AI route attached `reasoning_effort=max` to every model whose catalog said reasoning Supported. Z.AI documents that request field only for GLM-5.2 and above; older GLM models can think without accepting that separately version-gated control.
- Capability and control evidence are independent dimensions. PZA03 now selects three routes by canonical model id: effort + required continuation, effort + optional continuation, and no effort. Older compulsory-thinking models still require complete `reasoning_content` through provider-local replay/response guards, but receive no invented field.
- Aliases must choose on the resolved canonical model, not requested spelling; otherwise an alias can weaken a continuation rule or enable a control the model never documented. The remaining general-endpoint `clear_thinking=false` gap stays explicit because replaying state locally does not prove the server retained it.

## 206. A compatibility label does not replace the provider's response schema

- MiniMax's overview says to replay the complete Anthropic-compatible `response.content` list but does not enumerate every thinking child. Its concrete official response schema/example includes an opaque `signature` on the thinking block. The shared Anthropic adapter requires that continuation value for replay.
- Provider fixtures must follow the provider's actual compatibility response, not assume either “Anthropic-compatible means every Anthropic field” or “the overview did not name it, so it cannot exist.” PMM03 now requires and preserves the signature, rejects duplicate tool ids and cross-model/dialect replay, and keeps unknown blocks byte-semantically intact.
- This completes the row's fixture acceptance only. With no MiniMax inference adapter/plugin, no composed world consumes `MiniMaxStateRoute`; protocol correctness and product reachability remain separate claims exactly as AGENTS law #16 requires.

## 207. A zero-apply graph cannot diagnose an `apply` failure

- Static inspection and activation answer different questions. `inspect_composition` can prove descriptor identity, ordered injections and declared collisions without side effects; by construction it cannot observe an implementation that refuses inside `Plugin::apply`. Calling that report “activation health” made K08's acceptance impossible.
- Running the ordinary product world is not a safe diagnostic substitute: it may resolve credentials, resume/create sessions, write stores, start watchers or external MCP transports and leave every successful plugin live. K08 now keeps the original graph phase zero-apply and adds a separate production-loader probe in a disposable canonical workspace with temporary product state, fake inference, no watchers/resume and configured MCP transports suppressed.
- Isolation must stay visible. Human/schema-v1 JSON list the suppressed authority-bearing actions and name only plugin, scope, `activated|failed|not_attempted` and stable failure stage. Raw `CoreError` text is structurally absent because it may contain boundary data. A successful probe explicitly shuts down the Context before the temporary root is removed.
- The isolated phase proves that the selected plugin implementations can transact together under those declared substitutions. It does **not** prove credentials, live provider calls, persisted settings/resume data or external MCP servers are healthy; those retain their owning doctor/live-evidence rows.

## 208. Stateless Responses continuation is exact item replay, not assistant text replay

- With `store:false`, a later request owns all continuity. OpenAI's current reasoning guide says to preserve every output item for stateless `all_turns`, including encrypted reasoning and assistant phase. A neutral `ChatMessage::Assistant` has nowhere to retain the Responses item id, encrypted reasoning, call id or phase, so accepting it is a lossy protocol downgrade even if its text is identical.
- A generic protocol adapter can validate relationships among the provider-state items it receives but cannot know that a caller substituted a neutral assistant message or mutated an otherwise nonempty phase. `OpenAiProvider` therefore wraps the shared adapter and refuses neutral assistant history, empty encrypted reasoning and phases outside the closed `commentary|final_answer` set before transport.
- Hand-built replay fixtures cannot catch parser loss. The acceptance loop must consume the exact `ProviderStateItem`s emitted from streaming, then assert a later request preserves their order and opaque fields. Opening events deliberately omit encrypted content so using an incomplete event instead of the completed item fails deterministically.
- This proves provider-owned state continuity, not composed product reachability. Until CLI inference registration and credentialed evidence exist, POA02 does not turn an auth/catalog profile into a shipping OpenAI route.

## 209. Anthropic thinking mode is a route dialect, not one capability bit

- Adaptive and manual thinking accept different request shapes and continuation ordering. Adaptive mode interleaves automatically and permits assistant turns without a leading thinking block; legacy manual mode uses `budget_tokens`, needs the versioned interleaving header on supported models and requires the continued assistant turn to begin with thinking. Treating both as “reasoning supported” loses a live 400 boundary.
- During a tool-use turn, every `thinking` block and its opaque `signature`—or `redacted_thinking.data`—must return complete and unmodified. The guard applies independently to every interleaved step and ends only when a normal user message opens a new turn. Empty visible thinking can be legitimate; the opaque continuity field is the load-bearing value.
- Never let a manual constructor silently fall back to an adaptive-only default. PAN02 now requires an explicit model and rejects the known default. It does not claim arbitrary explicit ids are compatible: the Models API lacks a complete manual/adaptive/interleaving inventory, so that compatibility remains caller-owned/Unknown until stronger catalog evidence exists.
- Fixture completion is still not product activation. Provider registration, real account/model evidence and any model-aware route resolver remain separate rows.

## 210. Omitted gateway transforms are delegated authority, not “off”

- OpenRouter has both API defaults and account defaults. Omitting `plugins` delegates the decision to those layers; it does not disable context compression, PDF parsing or response healing. An all-off policy therefore serializes every known transform with `enabled:false`, and adding a transform must break an exhaustive match/test rather than inherit omission.
- Durable intent and wire projection are separate. Routing is one whole-object provider option mapped to top-level `provider`; transforms carry a sole nested `plugins` member mapped to top-level `plugins`. The shared Chat adapter now accepts multiple unique kind/field dialects, rejects duplicate kinds/target fields, and refuses member projections with missing or extra siblings so no durable value disappears on the wire.
- A compatibility constructor that silently omits the new option is still a bypass. Every `OpenRouterProvider` constructor now requires an explicit provider-owned transform option; the production factory supplies `OpenRouterTransformPolicy::all_disabled`. C02/C05 therefore persist and independently verify both routing and transforms before dispatch.
- Request intent is not execution evidence. OpenRouter accounts can prevent request overrides, so `effective` stays Unknown even after heycode sends an explicit disable. Costs likewise distinguish unpublished/variable Unknown from documented free and from token-billed work.

## 211. Recheck current provider docs before promoting recovered evidence

- A recovered MiniMax lane classified `understand_image` as legacy-only because one documentation index appeared to list only web search. The current Token Plan MCP guide now names **both** tools on the same page. The code was internally consistent and fully tested—and its evidence classification was still stale. Root acceptance must re-open time-sensitive primary sources, not merely audit implementation quality.
- The corrected policy has two honest choices: `AllDocumented` installs both current tools; `WebSearchOnly` deliberately reduces authority. “Current” versus “legacy” is evidence provenance, while “enabled” versus “disabled” is user policy. Never overload one to mean the other.
- Provider product names migrate too. MiniMax says Token Plan extends/replaces the former Coding Plan; current routes are ordinary `/v1` and `/anthropic`, and a Token Plan key may exist before an assigned seat or Credits makes it usable. Credential presence, product identity and entitlement are three separate facts.
- A secret-free launch description still is not an active MCP product. PMM04 stays active until a composed owner converts the definition, resolves the credential only at launch and proves connection plus both tool rows. PMM05 can complete independently because its acceptance is the explicit eligibility/profile boundary.

## 212. Provider-owned JSON is not durable until the session owns its fields

- PZA04 originally round-tripped all seven Z.AI search fields inside `ZaiWebSearchRecord`, then projected only URL/title/summary into shared events. That proves the provider type can serialize itself, not that a crash/resume or support bundle can reconstruct what the model saw. A provider-local “durable” payload with no session writer is an in-memory promise.
- The smallest shared fix is additive metadata on the already-durable `ServerToolSource`: site name, icon URL, provider reference and publication string are bounded/validated and optional. Old rows deserialize with metadata absent; Z.AI rows append through `server-tool/result`, reopen through the production session reader and retain every field. Debug exposes only presence flags, never URLs or summaries.
- Protocol code and product selection remain separate evidence. An opt-in composition-root plugin maps the provider-owned route into N01 without moving provider facts into the binary; exact inventory and effect disposal prove reachability. It still does not claim a Z.AI inference route or live credential.
- An MCP bundle has the same reachability test. Four exact secret-free specs plus rollback are useful substrate, but PZA05/PMM04 stay active while both HTTP and stdio connection owners reject unresolved credential references. Definition metadata is not a connected tool generation.

## 213. A model-control service is not yet a no-surprise-load product

- PLM04's provider boundary is now strong: planning performs no I/O, a load plan is consuming/non-Clone, every requested context/hardware setting must echo back exactly, unload targets an instance id, and `require_loaded` refuses downloaded-only models. Publishing `lmstudio/model-control` makes those operations reachable to future Consumers.
- The row's acceptance names a **settings command** and user control. Without U14 Settings/CAS, commands/UI, post-operation catalog refresh and inference-route enforcement, users still cannot invoke the boundary and another path can ignore `require_loaded`. LM Studio's global JIT setting can also surprise-load independently. The service is progress, not completion.
- Ollama compatibility has the symmetric identity trap. Its native `/api/tags` proves Ollama model inventory but not OpenAI Chat support; a separate compatibility profile/adapter and `/v1/models` check prove that route. Keeping `ollama` distinct from LM Studio is necessary, while production factory/picker wiring and a real-install chat response remain PLM05 acceptance gaps.
- Exact composition baselines are useful concurrency alarms. The PZA04 gate failed because the LM Studio lane added a real service after the baseline snapshot; updating the authoritative service-key list and expected owner was required, while rerunning unrelated tests before the lane settled was not.

## 214. Provider-owned metadata needs a shared application hook, not just a type

- AWS can validate cache checkpoints, guardrail settings and cross-Region/application targets inside `heycode-provider-aws`, but P07's Bedrock adapter will never send them unless it has a model-aware provider-option hook and exact wire mapping. Likewise, cache read/write/TTL details are not “visible” if shared usage keeps only an aggregate count.
- Google can classify grounding, code execution, cache provenance and Claude-on-Vertex evidence locally, but the current Gemini adapter rejects provider options and the current Messages adapter hardcodes its endpoint/body/header dialect. A correct provider projector beside an unmodified shared adapter is unreachable code.
- Keep three gates distinct: provider-local schema/fixture correctness, shared adapter/session/usage normalization, and composition/live evidence. The new lower layers move rows to active; they do not satisfy acceptances that explicitly say “visible,” “normalized,” “profile” or “live smoke.”
- A model-aware hook must receive the selected canonical model and resolved call, not infer from a provider-wide boolean. Otherwise a request option proven for one model/endpoint can leak into another and convert Unknown into accidental support.

## 215. Provider options are control metadata, not a second transcript

- DeepSeek prefix completion needs an assistant prefix containing ordinary code, including newlines. `ProviderRequestOption` deliberately rejects control-bearing/large model content; putting the prefix there would create a hidden second transcript outside the session projection. The prefix stays an ordinary assistant message and the option carries only a bounded boolean activation marker.
- Strict tools follow the same rule. Existing durable `ToolSpec`s are the source; the provider boundary projects `strict:true` into each function definition. Duplicating schemas inside an opaque option would risk desync and double-counting.
- Endpoint shape is capability identity. Strict tools and prefix use beta Chat, JSON Output uses standard Chat, and FIM uses beta `/completions` with its own 4K/non-thinking contract. One generic “DeepSeek optional features” flag would erase exactly the route differences the row accepts.
- Conflicting primary evidence stays Unknown. The FIM endpoint schema names Pro while current pricing tables discuss both V4 models; heycode may enforce Pro and leave Flash Unknown, but may not promote Flash because a marketing table looks broader.

## 216. A hosted-tool definition is not a normalized event path

- POA03 can define OpenAI web/file/code/shell/computer/image/MCP tools exactly and classify completed output items, while PAN03 can define Anthropic search/fetch/code/advisor/tool-search/MCP and preserve pause state. Those are necessary provider-owned facts, not proof that Agent/session/UI sees any call.
- Product acceptance needs a continuous path: selected model capability → request definition → provider parser → normalized call/result/citation/pause events → durable session → replay/UI. A provider crate prohibited from changing shared parsers can complete only the first and last provider-specific classifiers; its row stays active.
- Model tables change. Root/lane review caught Anthropic's current Opus 5 tool-search support replacing an older assumption; exact tri-state gates must follow the current primary table and keep older unsupported models distinct from Unknown.
- Credential-free remote MCP configuration means URLs/reference fields are safe to retain, not that the connector is authorized or live. Connection/auth evidence remains its own boundary.

## 217. A schema form is not a settings browser until it owns the CAS boundary

- S14 can derive safe controls from a schema, but U14 is the Consumer that turns those controls into product behavior. It must reopen authoritative namespace snapshots, carry their exact revision into `replace_user`, publish success only after persistence and surface a stale revision as a recoverable conflict. Mutating a renderer-local JSON copy proves nothing.
- Layer precedence changes editability. A project value outranks the user layer, so letting the browser “save” a user value beneath it would report success while the effective value stayed unchanged. Project, managed, secret, unrenderable and custom-delegated rows therefore remain visible with an exact reason rather than becoming misleading editors.
- Secret safety is structural across both layers: `SettingsField::Secret` exposes only `configured`, and `SettingsPanelValue::Secret` also has no value-bearing variant. The TUI cannot accidentally retain or render a credential because no secret string crosses the UI contract.
- A custom settings surface is a plugin registration, so it is an effect. `SettingsUiRegistry::register_custom` stores an opaque token and installs a disposer that removes only that exact row; after shutdown the namespace deterministically falls back to its derived form instead of retaining a dead plugin handle.
- Command registration is not reachability. `/settings` is accepted only because a production-composed `TuiHandle` shares its panel inbox with the command, the running shell attaches both `settings` and `settings-ui`, and a real-composition test follows command → request → browser rows. The new `settings-ui` service therefore belongs in the exact service-key/owner inventory.

## 218. Neutralize an external JIT setting at the request boundary; do not edit private config

- Current LM Studio primary docs expose “Just in Time Model Loading” as a server setting and explain its behavior, but publish no stable REST or CLI operation for changing it. An old collaborator comment explicitly calls the internal HTTP-server config unstable. heycode must not turn a private file into an authority-bearing API merely to claim it disabled JIT.
- PLM04 instead controls what heycode can cause. `/lmstudio load <model>` is the only product path to `POST /api/v1/models/load`; planning and Settings browsing perform no I/O. The operation rereads the live `lmstudio-load` snapshot, admits only affirmative tool-capable chat evidence, refuses an already-loaded row, verifies every echoed context/hardware value and confirms the exact instance in a fresh native list before publishing success.
- Unload has the symmetric boundary: the target must already appear as an exact loaded instance, the provider must echo that id, and a fresh list must show it absent. Model keys are never guessed into instance ids. Both successful operations force a shared catalog generation refresh, so the picker does not retain the pre-mutation view.
- A provider response is a commit point, but it is not the final product claim. Failures after mutation use explicit “accepted but readback unavailable/mismatch” or “succeeded but catalog refresh failed” classes. They must never be flattened into “operation failed,” which would invite a blind retry and possibly a duplicate load.
- Settings need an explicit omission vocabulary. Numeric mode fields distinguish `server-default` from a bounded value; boolean hardware controls distinguish `server-default`, enabled and disabled. Zero is not overloaded as “inherit,” and defaults never silently become sent values.
- The command is queued so an active turn settles before model memory changes. One Context-owned lifecycle token parents each operation wait; shutdown cancels it before effect-owned command and namespace registrations unwind.

## 219. A safe import preview minimizes values before it asks for authority

- Competitor configuration is mixed-authority input. Provider/model ids may be useful metadata, while `env`, headers, OAuth, API-key helpers and hooks can carry secrets or executable authority in the same document. Redacting their eventual rendering is too late: S13's public excluded/unknown rows have no value member, so the dangerous bytes cannot survive parsing into a preview or Debug output.
- Unknown fields still need visibility, but only as dotted path plus structural kind. This lets a user see that a future setting was skipped without retaining an unknown string that may itself be a credential. Known values pass both grammar/length checks and credential-shape screening before they enter typed rows.
- Authority is host-supplied, never inferred from a filename string. `heycode-config` deliberately does not depend upward on trust; `ImportAuthority` carries user/project and executable decisions from the future discovery owner. Project metadata can be previewed while untrusted but cannot become a candidate, and stdio MCP additionally needs executable authority.
- An enabled MCP row whose environment, headers, OAuth or working directory was excluded cannot be imported as a stripped server. Its readiness is `ExcludedMetadataRequired`, and candidate construction rejects the whole applicable set rather than applying a misleading partial configuration. Disabled rows remain visible but unapplied because the current root config has no disabled-state field.
- Candidate construction is detached and typed: clone an existing `Config`, refuse name collisions, clear route credential pointers and perform no I/O. Source discovery, UI confirmation, source-state CAS, atomic destination persistence and unresolved auth references belong to MCP14/later owners. S13 completes the preview contract without claiming those layers.
- Formats differ at the parser boundary. Claude settings are strict JSON; OpenCode accepts JSONC and trailing commas; Codex is TOML. Normalizing all three through a permissive parser would silently accept a document the source product itself rejects and make preview evidence unreliable.

## 220. Spill ownership and untrusted provenance belong below the LSP tool

- A body cap is not a complete model-output cap. E06 builds the wrapper, content id, total-byte metadata, escaped preview and footer inside one caller budget; the LSP Consumer returns `rendered_preview()` unchanged. Capping diagnostics first and appending a “read more” link later would exceed the very limit the spill service was meant to enforce.
- Complete bytes and previews need different authorities. Retained objects are SHA-256 addressed but lookups also require a validated logical owner, so knowing an id does not grant access. Foreign-owner and unknown-id reads share one class. The Unix backend verifies 0700/0600, regular-file/single-link identity and the full digest on every bounded read; non-Unix remains unsupported rather than borrowing weak ACL assumptions.
- LSP output is local but not trusted. A project-selected language-server process authors diagnostic messages and locations, so it gets its own `UntrustedContentSource::Lsp`, durable session annotation, native-runtime notice and TUI label. Calling it Web or MCP would lie about provenance; leaving it unlabelled would let a diagnostic become instructions to the model.
- Replaceable Providers need constructible output and error vocabulary. Public `LspLocation`, `LspDiagnostic`, `LspServerDescriptor` and fixed `LspError::from_code` constructors are necessary for a non-local backend to implement the public trait without private-field hacks. Their validators retain the same bounds as the stdio parser.
- Default composition can safely expose an empty LSP registry: `lsp_servers` returns `[]` and starts no child. Exact stdio definitions arrive only as effects from a trusted configuration/plugin owner. Definition/reference/diagnostic calls name a listed server id and independently re-resolve every path inside its workspace.
- One caller token cancels one JSON-RPC read and sends `$/cancelRequest`; it does not poison the reusable server. Registry/definition teardown cancels the session token, drops the sole raw process owner and reaps the process tree. No driver task is detached.
- Large LSP results cross the same durable tool-result plane as small results, but only the retained preview is model-visible in that call. The full object is available through owner-scoped bounded reads; a future read tool must preserve that owner rather than accepting one supplied by the model.

## 221. A loop limit is durable only when the session owns its exact reason

- Stopping before the next step and appending `turn/end error` is durable closure, but it is not a durable budget reason. A09 now extends the session-owned closed `TurnEndReason` set with max-steps, max-elapsed, max-tool-calls, unreported-usage and clock-unavailable; token exhaustion keeps max-tokens. Replay and restart can identify the exact policy stop without reconstructing a transient error string.
- Native runtime has a smaller settlement vocabulary, so every budget-specific reason maps to `Limit`; that normalization does not erase the session log. UI-safe error text still carries the exact stable code for the current caller.
- Budget counters must be projections, not mutable plugin state. Before every step the layer folds the active turn's durable step ends, usage-bearing assistant messages, dispatched tool calls and start time. A reopened session reaches the same exhaustion decision. A missing/duplicate start or overflow fails closed instead of resetting a counter.
- Defaults are execution policy and must be visible. Default `loop-budget-settings` owns restart-applied Settings with explicit step/token/millisecond/tool ceilings plus `require-reported|allow-lower-bound`. The default chooses lower-bound usage compatibility while retaining hard step/time/tool caps; deployments can choose strict reported usage without code changes.
- Installing a policy is an effect. The pre-step layer leaves before the owner marker on LIFO shutdown, and only while the marker exists does Agent suppress the legacy eight-pause fallback. Exact profiles that omit the plugin retain the compatibility cap rather than becoming unbounded.
- Deferred selection must not shrink today's ordinary product accidentally. `lexical-local` passes catalogs at or below its explicit 64-row boundary unchanged. Only oversized catalogs rank query overlap, preserve stable registry-order ties and cap selection. The same plan filters client schemas, N01 routes and prompt tool names; a selected name missing from the current union fails preparation.
- Code Mode is scheduling, not execution authority. Its type can materialize ordinary legacy chunks or strict inference events for selected calls, but imports neither ToolRegistry nor execution helpers. Every call still crosses A06 approval, cancellation and ordered durable commit.

## 222. A no-auth local provider must bypass the credential lane, not fake a credential

- Ollama's documented OpenAI-compatible API expects a conventional key value but ignores it; that wire compatibility constant is not a user credential. Startup therefore bypasses key presence/validation only for provider id `ollama`, while unknown and credentialed providers still take the normal lane. Adding an `OLLAMA_API_KEY` reference would turn a protocol placeholder into secret state.
- The bypass is narrow in both directions. An Ollama config carrying `api_key_env` fails before lookup and diagnostics never repeat the reference. `provider_key_present` returns true because no credential is required, not because a dummy secret was stored. The constructed provider exposes no credential reference.
- Local does not mean implicit. `provider-ollama` is registered only when config explicitly selects Ollama and an already-present model id; root may use the documented localhost origin or an explicit validated base URL, but never discovers, starts, pulls or chooses a model during composition.
- Product identity and inference reachability are a five-surface join: version, native tags, running state, per-model show metadata and OpenAI model listing. The catalog/profile/provider descriptors all agree on Chat only after that join. Native tags alone remain protocol Unknown.
- The conditional provider plugin publishes catalog/profile/inference/inspector services before the ordinary `llm` plugin bridges its concrete `OllamaInference` into the shared ProviderRegistry. This avoids a speculative late-mutable registry API and keeps every registration in Context ordering/inventory.
- Deterministic provider and real-composition tests prove the route/picker bridge without I/O at composition. They are not a live smoke. With no installed `ollama` executable/model on this host, PLM05 remains active rather than manufacturing a daemon, pull or chat response.

## 223. An unresolved auth reference must not invent the binding S13 erased

- S13 deliberately removes header names, environment names, OAuth client ids and all values. MCP14 may retain the safe source field path and classify a binding family, but cannot recreate an `Authorization` header or environment variable from convention. The deterministic `McpSecretReference` is a request for future human/management binding, not a usable transport credential.
- Clean rows and incomplete rows have different types of reachability. A clean authority-ready row can become an exact private `McpServerDefinition`; an enabled row with excluded metadata must make whole-set extraction fail. Applying its safe URL/command while dropping auth would create a server that looks configured and only fails later, which is both misleading and dangerous.
- Disabled incomplete rows remain visible with unresolved references and yield no definition. This preserves import intent without activating anything or blocking unrelated clean rows merely because a disabled future server needs binding.
- S13 authority is carried through, not recomputed. MCP14 has no second “trust” or “allow executable” knob that could widen a project/stdio decision. ProjectTrustRequired and ExecutableAuthorityRequired remain distinct actionable failures.
- Preview equality deliberately ignores excluded secret values. Changing only a token/header/OAuth value produces the same MCP14 preview/reference ids, proving no secret byte influences Debug, hashing or candidate state. Provider/settings/unknown-field values also never enter the MCP-specific preview.
- Definition extraction validates an absolute cwd, exact safe arguments/credential-free URL, reconnect-off posture and existing-name set before returning any candidate. A collision or one invalid enabled row returns no partial vector.
- A sandbox that forbids `TcpListener::bind` can block unrelated real-socket MCP tests. That is neither a pass nor an MCP14 failure: run the four pure import targets, record the three pre-existing socket cells as environment-blocked, and do not request unavailable escalation or loop on the same bind.

## 224. Advisory metadata provenance belongs on the value, not its enclosing generation

- A catalog generation timestamp proves when rows were fetched; it does not prove where or when a price/benchmark fact was captured once that value is copied, cached or rendered independently. Every non-empty `ModelPricing` and `ModelPerformance` now carries validated `ModelMetadataProvenance { source, captured_at_ms }`; Unknown carries none.
- Constructors enforce the invariant. `ModelPricing::unknown().with(...)` fails because a component cannot be added before provenance. Provider normalizers must parse components first, mint one honest generation capture, start with `captured`, then add the rest. The OpenRouter production normalizer initially missed this migration and would have rejected every priced live row; a provider-focused gate caught it before CAT06 acceptance.
- Persistence must preserve or deliberately discard provenance—never invent it. Catalog cache schema v2 writes source+capture inside each advisory object. Schema v1 remains readable, but its source-less pricing/performance restores as Unknown instead of borrowing the enclosing generation timestamp. Current schema rejects non-empty advisory data missing provenance.
- User catalog overrides are assertions beside provider evidence, never mutations of it. The override document cannot spell provenance; the loader mints source solely from the configured layer/path. An assertion can narrow, claim unevidenced support or contradict provider evidence, and each direction renders differently. It cannot add an unpublished model or leak into the provider cache.
- Per-field precedence matters. A project assertion for tools does not erase a user assertion for prompt cache, and each surviving field names its own layer. Malformed/duplicate/unknown/zero entries fail the whole layer set.
- A library `AttributedCatalog::render()` was not product visibility. Default `catalog-overrides` now loads user plus trusted-project layers and the TUI labels assertions, conflicts and unmatched rows. Raw provider capability filters deliberately do not admit a user `Supported` assertion, so it cannot masquerade as native evidence. CAT07 still remains active: asserted limits/capabilities are preview-only until the durable request context and C05 verification record their attribution before enforcement.

## 225. An opaque compaction checkpoint is route-scoped state, not a portable summary

- Provider-native compaction belongs below Agent as transport, not above it as session authority. `InferenceAdapter::native_compaction` consumes a resolved `CallPurpose::Compaction` call and returns a validated, bounded `NativeCompactionCheckpoint`; only the Agent registry may commit that value. Catalog support and operation availability are separate claims and both are required.
- Reusing `compaction/applied {summary}` for encrypted/provider blocks would make an older reader silently treat opaque state as text or empty history. C12 adds the closed v2-only `compaction/native` kind instead. V1 rejects it, and the event carries strategy, exact same-route items, boundary and optional normalized usage.
- Route-aware projection applies a native checkpoint only when provider, canonical model and protocol all match. A neutral projection or provider switch ignores the marker and retains the original append-only prefix; it must never discard history it cannot replay. C14 implements the explicit portable-recompact/pre-checkpoint-fork/cancel choice before route persistence.
- A compaction marker whose `replaced_upto_seq` names itself or a future event can erase arbitrary later history. Append, open and independent request projection all reject `replaced_upto_seq >= settlement.seq`; pure neutral folding assumes it receives a validated session.
- `/compact`, automatic pressure and native RuntimeSession compaction used to bypass the strategy registry independently. All three now select the same `portable-summary` row; shutdown disposes every row. Schema v22 inserts `compactions` before the first historical `agent`/`subagent` Consumer rather than hiding a default inside execution.
- Cancellation is not settlement if it merely drops a provider future that may own a task. The native strategy cancels the one operation token and awaits that future before returning; provider implementations must make that wait quiescent.

## 226. A compaction stress test must stress reconstruction, not just append speed

- Writing 1,000 user/assistant pairs proves almost nothing about C15. The durable fixture writes complete request header/context correlation, exact provider output state, neutral assistant copies and turn/step settlement for every turn, then alternates portable and native checkpoints. It reopens the actual JSONL and runs the production request projector over all 1,000 requests.
- Sequence contiguity is necessary but not sufficient. The final same-route input must contain the newest opaque checkpoint plus every retained provider item, proving compaction did not create an index gap or drop continuation state. `project_requests().len() == 1_000` independently proves earlier correlations remain structurally valid even when shadowed for current input.
- The opposite route is part of the stress. Projecting the same reopened log as Anthropic must contain only neutral messages, retain the latest human work and contain no OpenAI opaque item. Otherwise a native checkpoint can pass same-route tests while still leaking across providers.
- Do not turn a local 0.66-second run into a performance claim. C15 asserts structure and preservation, not latency; Q22 owns benchmark budgets and reference hardware.

## 227. Events after `turn/end` do not reopen the turn

- The original fold boundary guessed “mid-turn” whenever the final event was not `turn/end`. A native checkpoint is intentionally appended after a completed turn, so portable recompact saw that checkpoint last, popped the settled turn and returned `Noop`; the opaque barrier could never clear. Completion is a correlation fact, not a position: collect `turn/end` by turn id and include a start only when its own later end exists.
- Equal compaction boundaries need settlement order. A portable recompact may cover exactly the same prefix as the native marker it resolves. Keeping the first max boundary leaves native state winning forever; last-write-wins on equal `replaced_upto_seq` lets the later portable settlement supersede it without rewriting history.
- C14 resolves before Settings. A direct provider/model change crossing the winning native checkpoint returns an explicit-choice error. `cancel` writes nothing; `fork` uses `EventCount(native_settlement.seq)` so it includes all history immediately before the marker and leaves the current route unchanged; `portable` compacts with the current provider, rechecks the barrier, then and only then commits Settings/live selection.
- Fork is a durable alternative, not a disguised switch. `/provider <id> fork` returns the child id and keeps the current world untouched; the existing resume/session-lifecycle boundary opens it. App-server requests that cannot express a choice receive Conflict, never a silent fallback or provider body.

## 228. A provider-native operation is still unreachable until the selected production route owns it

- POA04/PAN04 lower clients and checkpoints existed before C12, but the CLI rejected both provider ids as configured-only. Provider tests proved protocol behavior; they did not prove a composed Agent could select the adapter. Root composition now constructs `OpenAiProvider`/`AnthropicProvider` from the isolated credential service and the same shared HTTP service, and real-composition tests assert `InferenceAdapter::native_compaction` plus the `provider-native` strategy without dispatching network I/O.
- A composition proof does not need a live key. Use a unique test-only credential reference in an owner-only temporary file, disable the sanctioned fake provider explicitly, compose through the production factory/loader and inspect the strict adapter. Never use process-global environment hooks or a real keychain item for this proof.
- Provider-local detailed metadata is not durable visibility. OpenAI cache read/write/reasoning facts and Anthropic applied edits/cache impact may be validated and held safely inside their crates, but POA05/PAN05/PAN06 could not complete until neutral session events/usage projection/settings/UI survived restart. A bounded in-memory response ledger cannot satisfy that acceptance; #237 records the later producer-policy closure.
- Provider-operated does not automatically mean exact. Anthropic's current token-count guide calls `/count_tokens` an estimate. `EstimationMethod::ProviderTokenizer` ranks ahead of the local UTF-8 ratio but still mints `EstimatedTokenCount`; leaving the descriptor `Exact` because the old shared enum lacked a suitable class would be an evidence lie.
- Default product policy remains portable compaction and cache/edit policies remain opt-in. Registering native support does not silently change `/compact`, invent a cache key, or enable context editing for every request.

## 229. A generated document nobody can reach is still not product documentation

- DOC03 already had the strong half: exhaustive `ModelCapabilities` destructuring makes a new field a compile error, provider route rows derive from the same selection constants as the binary, and a checked-in Markdown regeneration test catches drift. It stayed marked not-started because no root/ship/status entry point linked the artifact; correctness on disk was not discoverability.
- Link the generated file, not another hand-maintained summary table. README, engineering evidence and STATUS now point to the one reference, while FEATURES remains a comparative product matrix with different ownership.
- Static provider route classes can be generated deterministically. Per-model capability data cannot: it comes from credentialed live catalogs and changes by account/time. The reference must say this explicitly rather than checking one machine's cache into source.
- Regeneration is a sanctioned test-owned write (`HEYCODE_REGENERATE_DOCS=1 ...the_checked_in_reference_matches_the_generator`). The ordinary test remains read-only and red on drift.

## 230. An inspector must render evidence weakness, including the fact a plane does not exist yet

- U16 cannot print one attractive token total. `/context` renders each System/Messages/Tools/ProviderState/Attachments contributor as exact, estimated-with-method, or uncounted-with-reason, and preserves better-ranked counter refusals. A lower-bound denominator never becomes a percentage; headroom is “at most,” not a precise remainder.
- Price evidence and usage evidence weaken independently. A missing cached input price renders unknown, never free. `/usage` derives reported tokens/routes from JSONL; any unreported step makes the token total “at least,” and any missing route/usage/price makes reported cost Unknown rather than silently summing the rows that were convenient.
- Before #234 landed, valid provider cache/edit facts were not neutral durable facts. The inspector deliberately said `detailed cache: unavailable (no durable provider cache facts)` instead of printing zero or scraping provider-local ledgers. That visible gap kept U16/CMD07 and POA05/PAN05/PAN06 active at this checkpoint.
- Put the new commands in their own default `status-context` plugin, injected from `commands` and `models`, so historical exact profiles do not acquire a hidden dependency. Turn detail is bounded to the newest 20 rows; a 1,000-turn session cannot flood the terminal.
- `/compact` strategy visibility comes from the live registry. `list` performs no model/session write; `<strategy> [keep]` resolves the exact id; a lone numeric argument preserves the historical portable keep syntax. Do not hardcode three rows in the command.

## 231. A provider option is an exact object-to-wire dialect, not a bag of fields

- A durable `ProviderRequestOption` proves ownership and retains the complete provider object; it does not by itself prove a protocol adapter consumed every member. The Responses adapter now binds one option kind to an explicit member-to-top-level-field dialect and refuses missing or extra members before dispatch.
- Configuration fails before composition for empty/duplicate option kinds, empty/duplicate member names, duplicate target fields and collisions with adapter-owned fields such as `model`, `input`, `store`, `tools` or `metadata`. A provider option must never overwrite a protocol invariant because a mapping happened to use the same JSON key.
- Projection is all-or-nothing. OpenAI's cache key and cache options are siblings with one capability/policy decision; emitting only the key or only the options would create a request different from the durable header. The transport regression therefore asserts both fields on the production provider's ordinary `store:false` request.
- Native compaction is a distinct operation and does not inherit normal-call cache options. Purpose-aware option production remains necessary when Settings activates the policy; copying every provider default into every call purpose would make `/responses/compact` invalid.

## 232. Managed policy must inspect concrete descriptors before the first effect

- Profile scope and implementation source answer different questions. A built-in plugin enabled by a User layer has User activation scope but BuiltIn provenance; K11 constrains the descriptor source, not the layer that happened to select it. Exact id enable/disable remains the managed scope's ordinary K05 decision.
- Untrusted profile bytes cannot grant themselves policy authority. Schema v2 parses constraints, but `ProfileLayer::new` accepts them only with both Managed scope and a trusted `ProfileSource::Managed`. User, named, project, local-project and session layers fail before resolution. Schema-v1 plugin documents remain readable, while putting constraints in v1 fails instead of silently ignoring them.
- Factory names are insufficient for capability policy. `PluginFactories::build_profile` first constructs each selected implementation, then checks its classified `PluginSource` and every declared `PluginContributionKind`, and returns the vector only after all rows pass. It never calls `apply`; a rejected process/tool/provider has registered no service, effect or disposer.
- Descriptor under-declaration is not a bypass for classified plugins. Core's existing exact contribution inventory checks every committed row against the broad descriptor families during activation. Unclassified implementations can skip that cross-check only for compatibility, so a managed allowlist that admits only BuiltIn also excludes that escape hatch.
- The production loader must call the constrained factory API directly. A correct library validator behind an unused helper would not satisfy K11; real composition denies `external_process` through the same named-profile path and names the plugin/capability before a Context exists.

## 233. Session lifecycle safety is a root-wide lineage transaction, not a directory rename

- Archiving a shared-prefix session cannot move its directory: every descendant names that exact parent path and prefix hash. U15 uses a zero-byte in-place marker, and ordinary queries exclude it unless archived/all storage was requested. Invalid marker shapes fail the whole page rather than disappearing.
- A target file lock and a descendant scan do not close the cross-process race. Another process can fork after the scan and before the delete rename. Every `Session::fork` and query-owned delete/restore/export now takes the same owner-checked root lineage lock; delete holds it across descendant proof, exclusive target admission, rename and directory checkpoints.
- A lossless export cannot call `read(path)` after validating a `Session` and assume those bytes are the validated stream. Another shared writer may append between the two. Each physical suffix is re-hashed against the exact opened event leaves; a mismatch fails instead of exporting an unverified or partial generation.
- Destructive publication follows the storage commit. Delete starts on Cancel, lower-layer current/open/ancestor refusals are independent of the UI, a successful move syncs trash plus sessions root, and only then does the UI expose the recovery receipt. Off Unix, destructive mutation remains Unsupported until an equivalent audited locking backend exists.
- A typed recomposition outcome is not a second loader. New/resume/fork return only an already-created or revalidated `SessionId`; the CLI replaces all prior resume selectors with the exact store path, preserves every unrelated argument, shuts down Context/runtime and re-enters startup. Schema v23 repairs exact TUI profiles with the newly mandatory query owner.
- Lossless JSONL and human Markdown are not a redacted support bundle. CMD06 can complete those explicit operations while C10 remains open for redaction/support packaging; documentation must not merge the claims.

## 234. Detailed provider usage needs a neutral durable fact, not an in-memory side ledger

- Normalized `TokenUsage` cannot carry cache reads, writes, TTL partitions, reasoning subsets or applied context edits without erasing distinctions that affect cost and pressure. `ProviderResponseMetadata` is a schema-v1 core value with exact optionality; missing reasoning remains absent, never zero.
- Provider-local parsing is still the authority for wire dialects. OpenAI maps current Responses details; Anthropic maps its already-validated cache/edit report. Both emit one neutral `ResponseMetadata` before Usage/Finish. Agent verifies detailed totals against normalized Usage and buffers the fact with other strict output, so failure/abort commits nothing.
- The durable event is v2-only `assistant/response-metadata`, correlated by request/turn/step. Independent request projection rejects orphan, duplicate, mismatched and invalid rows, while provider input projection ignores it. A restart inspector reads the projection, not an adapter's bounded response-id ledger.
- Cache-aware price arithmetic requires a partition, not subtraction by intuition. Anthropic proves uncached + read + write = total. OpenAI currently reports cache subsets without a neutral non-overlap proof, so a non-zero cache response remains Unknown cost even when every price exists; double-billing is worse than an explicit Unknown.
- The inspector distinguishes absence from corruption. No metadata renders unavailable/no durable facts; a projection failure renders invalid durable metadata. Both `/context` and `/usage` bound detailed rows and show cache-prefix invalidation plus cleared units/tokens.
- A shared Consumer can complete before every producer policy is default-reachable. U16/CMD07 faithfully rendered any committed provider facts while POA05/PAN05/PAN06 remained active because their opt-in Settings producers were still absent from production composition, not because the durable/UI plane was missing. #237 closes that later boundary without changing this sequencing lesson.

## 235. Managed plugin admission must freeze bytes before policy and recheck before activation

- One “trusted marketplace” boolean cannot express PL08. Source, channel, publisher namespace, exact version, catalog/package digest, signature posture, platform and capability ceiling are independent decisions; only eight affirmative Allowed verdicts authorize an operation. A missing rule is eight denials, not a permissive fallback.
- PL05 signature presence is not verification, and a manifest checksum is not an upstream fetch receipt. A policy requiring verified authorship or verified upstream artifact bytes yields Unknown and therefore denies. Calling syntactic metadata verified would turn a requested security property into marketing.
- Policy must run before cache mutation over the exact bytes later committed. `prepare_directory` freezes and hashes the source tree; managed admission evaluates that manifest/hash, then `commit_prepared` publishes those same bytes without reopening the ambient path. A denied/substituted package creates no lock, stage, object or reference.
- Installation-time approval cannot authorize future activation forever. `prepare_managed_declarative` resolves and rehashes the immutable object, reapplies current PL05 provenance plus current managed policy, and freezes contribution documents before any host callback. Only its opaque result enters the managed activation wrapper.
- A correct lower boundary is not product enforcement while ordinary lifecycle state can enable cached packages without calling it. Production `PluginLifecycle` now requires an admission provider for install/enable/update/rollback: absent managed authority denies before mutation, while disable/remove remain available. `ManagedLifecycleAdmission` rechecks a supplied exact generation. Low-level PL06 tests may still use the explicitly unmanaged constructor, but the product composition root does not.

## 236. Redaction is strongest when sensitive values have no output field

- Running a credential regex over copied session JSON is not a redacted support bundle. Prompts, responses, reasoning, tool args/results, provider state/options, citations, titles, paths, attachment names and opaque ids can all be sensitive without matching a credential format. C10 never serializes them.
- The support trace schema retains only static event kind names, sequence/time, closed outcome names, counts/booleans and numeric usage/cache/edit facts. Even the session id and provider/model ids are absent. A real canary session asserts the forbidden strings cannot appear because no code path reads those values into the output.
- Boundedness needs an explicit omission fact. The trace keeps at most the newest 10,000 logical events and 8 MiB, records the exact omitted-prefix count, and refuses a shape that still exceeds the byte ceiling. Lossless JSONL and human Markdown remain distinct explicit formats; redaction never changes their contract.
- A session trace is not the complete support workflow. C10 owns the three session export forms. Q18/CMD12 still own previewing and joining redacted config descriptors, plugin inventory, health history/version/platform and transmission approval.

## 237. Provider policy and its pressure estimator must resolve the same Settings generation

- Registering cache/context types is not a user control. `provider-openai` and `provider-anthropic` now own restart-applied Settings namespaces, both default disabled, and production provider construction resolves the committed snapshot before registry publication. There is no constructor default that silently enables spend/retention behavior.
- A prompt-cache key is not an API credential, but it is sent to a provider and persisted in request options. OpenAI constrains it to a bounded identifier, applies the shared credential-material screen, explicitly attests the public wire path and redacts it from Debug. Disabled accepts an empty placeholder but sends nothing; enabled requires a real key.
- Anthropic omission needs modes, not zero. Every numeric control has a positive visible value plus `provider-default|disabled|explicit` mode; 5m/1h cache, thinking keep-all/turns, input-token/tool-use triggers, tool keep, clear-at-least, input clearing and exclusions map to exact provider types. Unknown/zero/stale shapes fail the whole namespace.
- Pressure and inference cannot read different context-edit/cache plans. `token-count-anthropic` resolves `AnthropicSettingsPolicies` from the same registered generation and applies both policies to its request configuration before registering the counter.
- Namespace ownership changes dependency law. Provider plugins now declare Service plus Provider, exact inventory includes both namespaces, and schema v24 inserts `settings` before historical provider owners; a counter-only Anthropic profile also gains authorization/settings/provider owner before the counter.

## 238. Provider middleware must sit inside the durable request/response boundary

- A generic mutable request hook is unsafe if it can rewrite provider/model or input chronology after those facts were projected from JSONL. P10 exposes them read-only. Only system, tools and option fields independently captured in `request/header` are mutable; adapter validation, durable append and C05 comparison still precede transport.
- Around-middleware needs a mechanical way to distinguish policy from omission. `Waterfall::run_checked` records whether execution reached the terminal delegate. A validated kebab policy code may deliberately reject; returning without `next` and without that verdict is an undeclared short-circuit and fails closed.
- Layer errors are an untrusted diagnostic boundary. The interception service discards their bodies and returns only request/response plus `layer failed`; a plugin cannot smuggle a credential, provider response body or URL into turn/UI errors through `anyhow`.
- Response interception belongs before every publication plane. Normalized events and only the body-free `ProviderErrorClass` for failures are admitted before accumulator validation, telemetry, session append and UI delta. A rejection cancels the same adapter operation token and closes the step/turn with no rejected output event; cancellation waits for a parked layer to settle.
- “Auth uses the seam” requires an independently checkable fact. Every strict adapter publishes a secret-free `AuthenticationBinding` preview; request layers can admit it, and Agent refuses if the final resolved call changes it. Native-tool candidates similarly pass a post-`next` registry check plus a final direct recheck, so an earlier layer cannot mutate after a downstream validator and inject a stale route.
- Telemetry remains optional and content-free. `provider-telemetry` records the existing closed `RequestFailed` event from failure class/provider/model labels only; the original `LlmError`, response body and URL never enter the layer. Local-off counts with no exporter, while the OTLP replacement supplies the same service contract.
- Scope must remain truthful. Strict C02/C05 conversation dispatch and native child Agents share the global service. Legacy Chat compatibility lacks the exact durable request plane, and provider-native compaction has a different response type; neither is silently claimed as intercepted.

## 239. Profile precedence and activation order are different resolutions

- A profile layer may legitimately replace a default provider with a plugin id that was not in the base order. Scope resolution appended `telemetry-otlp-http` after the base rows; `provider-telemetry` therefore appeared first and correctly failed its `telemetry` inject. The profile choice was valid, but the activation order was not.
- Concrete factories now perform one stable topological order over exact `provides`/`injects` after scope resolution and managed admission. Providers precede their Consumers; rows with no dependency relation retain original index order. This is an explicit boundary default, not a retry inside `apply` and not a binary special case for telemetry.
- Missing providers still reach composition as missing injections, duplicate providers still fail exact service ownership, and a service dependency cycle fails at config construction naming the involved plugin ids. Reordering cannot turn an unknown/missing service into a default.
- Scope attribution travels with the `ScopedPlugin` while it moves. A Managed provider ordered before a User Consumer remains Managed; dependency repair must never rewrite who authorized either row.

## 240. A transform registry records requested policy, not provider execution

- OpenRouter plugins run once around a request/response; server tools are model-invoked zero-to-many. The deprecated `web` plugin stays outside N05 because POR05 already owns `openrouter:web_search`; merging them would double-authorize one capability and erase call semantics.
- Every registered provider contributes its complete enabled and disabled row set once. The registry freezes validated descriptors at effect registration rather than calling provider code under its lock on every snapshot; dynamic descriptor drift cannot silently change inspection or deadlock by re-entry.
- Disabled has no cost record. Enabled must carry one explicit cost evidence value: Unknown, documented free, upstream input-token billing or a validated non-zero per-thousand-page fee. Unknown and free are never equal, and zero cannot manufacture a published-free claim.
- The P10 layer inserts the provider option when absent, accepts a byte-semantically equal existing option and refuses a same-provider/kind conflict. It never overwrites the durable provider policy. Adapter validation and C05 still compare the final option before transport.
- OpenRouter's current rows remain honest: context compression may truncate/reroute and has unpublished cost; file parsing carries engine-specific free/input-token/$2-per-1,000-page evidence; response healing rewrites output, is documented free and requires non-streaming structured output. Account “Prevent overrides” means effective state remains Unknown even when heycode explicitly enables or disables a row.

## 241. An aggregate provider count is usage evidence, not a synthetic call

- OpenRouter reports `usage.server_tool_use.web_search_requests`, but its Chat output does not expose each query/call id/result. N06 emits a distinct validated aggregate event and still emits no `ServerToolCall`; turning the number 2 into two invented calls would corrupt replay, approval and outcome attribution.
- Local calls, exact provider calls and provider aggregates are separate usage planes. `/usage` groups by `(source, logical)` and never sums an aggregate into exact calls, because a future provider may publish both views of the same work.
- Exact call outcomes derive only from durable call/result ids. Duplicate calls/results or duplicate request/logical aggregates cannot inflate the projection; request projection separately fails the invalid log while the infallible usage view keeps first evidence only.
- Cost needs evidence as explicit as count. Aggregate usage carries Unknown or a validated published non-zero pico-unit total. The current OpenRouter `auto` engine may choose native, Exa or another engine with different pricing, so its exact request count still has Unknown cost. Unknown is not free.
- Support export and UI retain only source/logical identifiers, counts, closed outcomes and numeric cost evidence. Queries, arguments, tool output, provider bodies and citation content never enter N06 telemetry/usage rows.

## 242. Committed telemetry listens where the commit owner publishes

- `SessionEvent` is published by the session-owned bus after append succeeds; the Context event bus is not an alias. A TEL04 listener on `Context::events()` composes cleanly and records nothing, which is more dangerous than a loud injection failure. Consumers of durable facts must obtain `Session::bus()` from the injected session service.
- Startup replay and live metric emission are different operations. The listener seeds lineage, runtime and bounded request-route correlation from existing JSONL without emitting; only events observed after registration become metrics. Re-emitting the prefix on every resume would multiply historical counts.
- Aggregate telemetry needs a count in the wire type. Schema v2 adds one positive count, local counters and OTLP delta sums use it, v1 reads as one and zero fails. Converting `web_search_requests = 7` into one event would preserve type shape while losing the measured fact.
- Labels remain a closed projection: provider/model/purpose/lineage/runtime, local/provider-exact/provider-aggregate execution, compaction kind and cache activity. Prompts, arguments, results, paths, response bodies and arbitrary error text have no dimension or payload field.
- Metric registration is an effect and is inventory-visible. `telemetry-metrics` injects session plus whichever telemetry provider the profile selected; stable dependency ordering makes both local-off and opt-in OTLP use the same Consumer without a provider-specific branch.

## 243. An accessible renderer is not a product mode until startup can select it

- A screen-reader snapshot must be a second projection of the one production `AppState`, not a parallel interaction model. Trust, secrets, onboarding, approvals, pickers and panels keep the same modal priority and key router; the flat renderer only names that state linearly.
- Flat means no cursor protocol. It writes no alternate-screen, cursor-show/hide, color or animation bytes, suppresses unchanged frames and strips terminal control characters from state-derived text. `TERM=dumb` selects it automatically; this is capability handling, not a test hook.
- A public lower method alone did not complete U19. The shipping CLI now owns explicit `--screen-reader` selection, rejects it outside interactive TUI mode and preserves it through connection/profile/session/trust recomposition. Product reachability is a separate acceptance fact from renderer correctness.
- Q03 journeys use explicit typed host actions, terminal events and UI events against the production reducer. Fixed trust/setup/command/MCP/provider fixtures and exact flat frames avoid clocks, network, subprocesses and terminal timing while still testing real focus and keyboard transitions.

## 244. Per-operation support below the composition root does not prevent a frozen production key

- Chat, Responses, Messages, Gemini and Bedrock already acquired `RouteCredential` once per operation, yet the production LLM factory first resolved `CredentialsService` and passed the resulting String into fixed-key provider constructors. Every adapter test passed while the shipping four API routes still required recomposition for rotation.
- Startup preflight and execution ownership are separate. Preflight may resolve to diagnose presence/validity and bind the private validation fingerprint, but that value is dropped. The provider retains `RouteCredential::registry(service, exact_query)` and each operation acquires once; retries share that one value so a retry storm cannot become a keychain-prompt storm.
- Route identity must be checked before registry access. A resolver bound to Anthropic cannot answer an OpenAI handle and must not even inspect the credential service; otherwise “fallback” can both send under the wrong account and disclose which neighbouring references exist.
- Product reachability is proven independently from the protocol matrix. Shared transport fixtures prove rotation and no-fallback behavior; real production compositions for DeepSeek, OpenRouter, OpenAI and Anthropic prove their strict authentication previews carry each configured custom reference rather than `AdapterOwned`.
- Fixed credentials remain an explicit embedding/test boundary. Their authentication evidence is `AdapterOwned`; they are not a substitute for the registry-backed production factory and should never be introduced by a composition default.

## 245. Composer keys choose durable delivery, not transcript decoration

- During a native active turn, Enter is Steer and Tab is FollowUp; Esc remains cancellation. The TUI queues the exact private text through `Agent::submit_inbox` and narrates only the safe delivery class. The transcript receives the text only when Agent's claim atomically appends the splice removal plus `user/message` and then publishes `UserEcho`.
- UI liveness can lag Agent liveness by one event-loop boundary. If Enter was interpreted as steer but the Agent has already settled, `submit_inbox` truthfully returns `Wake`; a next-step item would have no turn to drain it. The TUI cancels that occurrence durably, requeues the same text as FollowUp and preserves both the race and the intent instead of stranding or silently dropping it.
- One Wake has one owner. The TUI stores the wake until both turn and command tasks are settled, consumes it once, starts one caller-token-owned follow-up turn, and waits for settlement to publish another wake if more next-turn messages remain. Follow-ups take priority over post-turn queued commands because they are already durable user input.
- Resume must seed the live control plane from the durable inbox projection. Pending next-turn input wakes automatically; pending next-step input stays visible until a real native turn can drain it. Replaying transcript cards alone would hide work that still exists.
- A composed native Agent is not a fallback for a delegated runtime. Until the stable app-server exposes provider-native steer/follow-up controls, Codex/Claude active composers keep the text and say the control is unavailable. Sending it to the native inbox would target a different session while appearing successful.
- Key meaning is state, not a hard-coded footer. `queue-follow-up` is a persisted/rebindable keymap action (Tab by default); full and screen-reader renderers show active native Enter/Tab/Esc semantics and exact queue counts, while idle Tab remains ordinary composer input.

## 246. A global child registry is lifecycle ownership, not caller authority

- O01 correctly retained continuable handles in one effect-owned registry, but its model-facing `children`, `child` and `interrupt` lookups were global. A nested child sharing the ToolRegistry could list or control its parent's sibling. Keeping all handles alive in one place solves disposal; it does not answer who may name them.
- `SubagentAuthority` now carries an opaque owner, depth, retention right and a private registry token. Only the registry mints a root; child authority derives the same unforgeable token with depth+1 and the new child session id. An unbound request fails before provider selection/execution, so a caller cannot reset depth by constructing a public request with zero.
- Every retained handle stores its owner. List/send/interrupt/close require the current authority and foreign ids behave exactly like unknown/finished ids. This is both isolation and non-disclosure: a child cannot learn that a sibling exists by probing its id.
- Lifetime constrains delegation. A one-shot child may run nested one-shot work but cannot create a continuable descendant that would outlive its authority. A continuable child's follow-up scopes the same retained authority, so a later `send_message` cannot reset depth. The async tool scheduler uses in-task `FuturesUnordered`, preserving Tokio task-local authority; changing it to detached `tokio::spawn` would require explicit authority propagation first.
- Provider output is boundary data. A one-shot request returning a handle, a continuable request returning none, a mismatched handle id or a duplicate live id fails before publication; any returned handle is closed on rejection. Capability evidence alone is not permission to accept an incoherent owner graph.
- O02 context modes remain distinct: Fresh is a new durable `Subagent` session and sees only its prompt; ForkParent is a hash-verified shared-prefix fork with one local prompt; Continuable retains the same child Agent/session. No copied-history marker or parent transcript echo substitutes for lineage evidence.

## 247. “Providers return redacted errors” is not a redaction boundary

- `CredentialProvider` historically returned `Result<_, String>` under a documentation contract that the String was safe. `CredentialsService` copied it into `CredentialsError::Provider`, then `resolve_route` copied that rendered error again. A faulty or third-party Provider could return the credential itself and reach logs/UI through an ordinary authentication failure.
- Provider failure bodies are now treated like network response bodies: discarded at the registry boundary. Public errors retain only the validated provider id and requested non-secret reference. A future need for actionable detail requires a closed typed code, not reopening a free-text escape hatch.
- A canary test must prove its premise at every layer. QSEC02 checks that the map-backed environment provider really resolves the canary, the mock transport really receives it in the Authorization header, and a purpose-built `ProcessSpec` really holds it before asserting the respective Debug surfaces are clean. An absent string with no positive premise is decoration.
- One journey is stronger than unrelated local assertions. The same canary crosses operation-time credential acquisition and strict Agent dispatch, then is checked against provider body/system prompt, UI/debug, request projection, physical JSONL, process diagnostics and the committed structural support export. Only the exact outbound auth header may observe the value.
- Session redaction and session truth are different claims. Ordinary lossless JSONL may contain user/model/tool content, but it must never acquire a credential solely because the request authenticated. `RedactedSupport` goes further and has no arbitrary content field at all. QSEC02 tests the credential-origin case; C10 separately tests arbitrary sensitive session content.

## 248. A replay oracle must reopen bytes, not re-project the writer's memory

- C05 already compared a projected request to its live `ResolvedCall`, but provider fixtures rebuilt tiny in-memory event vectors independently. Q04 now gives every fixture one shared operation: commit C02 snapshots, flush, reopen JSONL, find the exact request id, project, then invoke the production C05 verifier and exact adapter.
- Reopening needs a mutation that can actually survive the live writer. The first corruption test appended `not-json` through a second file descriptor; `Session` still held its earlier write cursor and its next header/context writes overwrote that tail, so the oracle correctly succeeded. A test mutation that the system erases proves nothing.
- Overwriting an already persisted envelope byte is the effective control. The in-memory Session remains valid while the physical reader fails, so an oracle using memory would survive and the real reopen returns the body-free `Reopen` class. Error text contains no system prompt/provider state/schema bytes.
- The reusable boundary lives in `heycode-agent::testing`, the lowest crate that can legally join Session projection to LLM adapters. Moving it into `heycode-llm` would violate the dependency rule; duplicating it in provider crates would let their interpretations drift.
- The protocol matrix covers Chat, Responses, Anthropic Messages, Gemini GenerateContent and Bedrock Converse with one fixture contract. Success returns `VerifiedResolvedCall`, the same ownership-consuming dispatch capability production uses; the oracle does not invent a weaker “looks equal” result.

## 249. A panel fixture is not a production command path

- MCP and plugin panels both had detailed reducer/render tests, and `/mcp` had a bridge test, while the shipping loop never attached either Settings-backed operations handle. Every local panel test passed and the actual command remained unavailable. Product reachability needs the composed service to enter `LoopDeps`, be attached before dispatch and reach both full and flat renderers.
- Command ownership must survive navigation. `/plugins` still owns exact inventory and `/skills` still owns its immutable catalog; moving either id into a TUI plugin would create a duplicate or erase non-TUI behavior. A validated `UiPanelId` is the human-only cross-front-end control event. It never becomes session/model/runtime output.
- A capability panel must not invent a second data source. Skills render the live `SkillSet`, agents render provider descriptors plus only an aggregate child count, hooks render owner/event/phase/handler class without commands, prompts or arguments, and MCP/plugin panels reuse the same Settings-backed owners as their standalone CLI surfaces.
- Availability and priority are state, not decoration. Panel-inbox commands report an absent service in the command catalog, unknown owner events fail visibly, and trust/secret/onboarding/approval/question surfaces prevent or close lower-priority panels. Full-screen and screen-reader output consume the same bounded control-free view.

## 250. A rebuildable index must prove both disposability and source stability

- “JSONL remains truth” means compare cannot answer from SQLite first. It reopens every active and archived root/fork stream through the normal bounded lineage reader, projects safe summaries and exact logical-line hashes, then decides whether the database is current. Corrupt SQLite is a rebuild reason; corrupt JSONL is a hard source failure.
- Rebuild needs two source observations under the existing lineage-mutation lock: one before writing the private staging database and one before publication. The last-good index remains byte-exact on invalid truth or projection failure, and owner-only atomic replacement happens only after both projections match.
- Migration evidence is a matrix, not two convenient examples. Q17 checks unversioned config plus every schema through current, byte-idempotent re-planning/application, byte-exact backups and truthful downgrade guidance; future config/session versions fail unchanged with the supported range. Session v1 history is never rewritten—its next append is v2.
- Generated default profiles are a separate migration premise. The matrix passes the live profile explicitly when testing a historical setup snapshot, so newly required rows such as `plugin-lifecycle` and `panel-commands` are named by the ordinary `UseBuiltinProfile` change rather than hidden in a one-off schema branch.

## 251. A permission prompt is a correlated operation, not a boolean callback

- ACP ask-mode previously had cancellation/rich-event plumbing but no real tool permission round trip. X01 now parks the exact tool approval behind one server-originated JSON-RPC request. The pending outbound id owns the oneshot; a wrong id is ignored, the first exact id removes the row, and duplicate/late responses cannot reach the already-settled tool.
- “Allow” is not any truthy-looking payload. The client must select the exact offered `allow_once` option with outcome `selected`; deny, cancel, missing option and contradictory selected/cancelled shapes all resolve to denial. Each path proves tool execution count and a subsequent prompt proves no poisoned session state.
- A delegated ACP runtime needs one process owner per connection, not raw command spawning in the kernel. `AcpRuntime` accepts a caller `AcpProcessFactory`, validates exact absolute program/cwd/argv/explicit environment, owns bounded fragmented NDJSON and closes the abstract process exactly once on handshake failure, cancellation or session close. R10 still owes the heycode-exec-backed production plugin and live Ox evidence.
- Interoperability and product proof remain different. The generic runtime and a VS Code-shaped SDK client fixture prove framing, catalogs, selection, normalized events, permissions and cancellation. They complete R11 and support X07, but do not manufacture an installed OpenCode or real VS Code extension run.

## 252. A checked-in signing workflow is neither a verifier nor an observed release

- Signature metadata in a manifest is attacker-authored until a configured verifier authenticates the exact raw manifest/artifact bytes, bundle, repository, workflow and OIDC issuer. The install boundary therefore owns non-clone `VerifiedReleaseArtifact` bytes minted only from an attested manifest; public Debug/Errors never carry bundle, verifier or executable content.
- Publication needs a recoverable transaction, not `rename` followed by hope. The update journal binds current/candidate/digests and recovery phase, last-good artifacts remain retained, and install/rollback re-verifies checksum plus signature before switching. Directional config migration may refuse rollback and require the byte-exact backup; it is never reverse-applied.
- Release channel and plugin compatibility are admission facts. Stable rejects prereleases/backward movement, preview admits newer prereleases, pinned selects exactly one version, and every enabled plugin's inclusive host-API range must accept the candidate and rollback target. A future product Consumer must derive that set from the live PL01 generation rather than a hand-built list.
- GitHub OIDC/attestation definitions avoid long-lived signing secrets, but a workflow file has run zero times. Q16 separately types `workflow_definition`, `cross_compiled`, local native and non-zero hosted-native evidence; even three green native fake turns do not satisfy the required real-provider turn on macOS, Linux and Windows.

## 253. OAuth client discovery is an authority chain, not a menu of equivalent defaults

- MCP authorization binds the protected resource, selected authorization-server issuer, redirect URI and client registration into one object. Pre-registered information has priority, CIMD is a document-bound public client, and DCR is a bounded fallback only when advertised. A client id or token endpoint from another issuer cannot be reused because it happens to parse.
- Callback acceptance checks exact redirect, state, optional/required issuer and code before exchange; exchange sends the exact resource and S256 verifier, never the challenge. Client secrets have no Debug/public URL slot and reach only the outbound token endpoint authentication boundary. Token records bind resource+issuer+client and stale bindings reload as absent.
- A protocol lab must share lifecycle assertions without erasing transport facts. Stdio, Streamable HTTP and OAuth each supply success/cancel/hostile observations; the runner requires one atomic publication, bounded requests, zero cancellation publication and a body/canary-free hostile diagnostic. This completes Q05 but not MCP15's official inspector/real server evidence.
- MCP annotations are server assertions, not permissions. Known hints must be booleans, unknown fields stay bounded advisory data, and exact enabled/disabled/per-tool approval policy wins without consulting a claimed `readOnlyHint` or `idempotentHint`.

## 254. A successful external registration must return its disposer

- PL03's host trait formerly returned `()`, so a host could report success and forget to attach any effect. Every declarative skill/command/agent/hook/theme/provider activation now returns an exact `DeclarativeContributionRegistration`; the bridge immediately gives its one-shot withdrawal to Context. Partial failure therefore rolls back the exact prefix without trusting host discipline.
- PL09's code boundary authenticates package and executable digests separately, denies all capabilities by default, refuses unrequested grants and admits one complete six-kind contribution generation only after a strict correlated handshake. Host refusal kills the process before composition returns; crash/cancel retires the whole generation, while a remote invocation denial does not erase a healthy process.
- An abstract launcher plus fake process is protocol evidence, not product reachability. Until a heycode-exec-backed launcher and concrete domain registry adapters compose under a profile, PL03/PL09 stay active. WASI/WIT work has not started, so PL10 remains not-started rather than being implied by a protocol that could host it later.
- Management handles are lifecycle data too. MCP and plugin-lifecycle plugins now publish their Settings-backed services through Context and flip held handles terminal on rollback/shutdown; the TUI consumes these services instead of constructing hidden binary-owned copies.

## 255. A user catalog assertion can constrain evidence but cannot create it

- Visibility and enforcement need different projections. The UI renders the assertion's layer/document, provider revision/capture and computed direction; using the machine value for display would make a user's guess look vendor-published. The machine accessor retains origin and accepts only redundant or narrowing assertions.
- `Supported` over provider Unknown or Unsupported falls back to the provider evidence for execution. A larger limit or any limit over Unknown does the same. An asserted Unsupported or smaller limit may narrow because it can only make heycode decline work; contradiction and unevidenced-support warnings remain visible.
- Fixture provenance is mandatory boundary data. CAT08 schema v1 requires a safe provider id, official-example/redacted-capture/synthetic kind, credential-free absolute HTTPS source, bounded source version and non-zero capture/reconciliation time. Synthetic protocol data stays synthetic through every fragmentation case and cannot be advertised as a provider capture.
- Current OpenRouter GLM fixtures name the exact official list/detail sources and capture instant; DeepSeek Anthropic parity fixtures explicitly remain synthetic. Fixture metadata does not promote a live-only provider row.

## 256. A WebSocket fallback cannot replay after the connection commits

- No current Chat/Responses/Messages/Gemini GenerateContent/Bedrock Converse route has a WebSocket wire. OpenAI Realtime and Gemini Bidi are different protocols. P13 therefore defines an optional connector seam and proves fallback/reconnect policy without adding a library or claiming a provider endpoint.
- Fallback is legal only before a connection opens and only when HTTP/SSE preserves the caller's server-to-client requirement. A bidirectional requirement with no connector or exhausted opens fails; it never silently loses client-to-server capability.
- Reconnect covers the handshake only. Opening frames are bounded and sent once after connection; a send or stream failure is terminal because another socket/HTTP request could duplicate work. Protocol-level resumption needs sequence identity owned by that future adapter.
- Outcome and metrics independently retain attempts, opens, reconnects, fallbacks, sessions, stream failures and cancellation. An unpolled session records nothing, and a cancelled backoff settles without waiting out its delay.

## 257. Documentation freshness needs authoritative projections and evidence labels

- Volatile tables should be generated from the code contract that would fail if they drifted. Commands come from the exact real-composition catalog plus source descriptors; configuration comes from structs/defaults/schema and Settings inventory; plugin docs come from built-in order plus closed descriptor/manifest vocabularies; session kinds come from the closed v1/v2 reader lists and enum docs.
- Generation and verification are separate operations, deliberately. `verify_docs.py` renders in memory and compares against the committed bytes without writing, so a stale reference fails; `--write` regenerates. A gate that rewrites the bytes it is about to compare can never fail — see `crates/heycode-cli/src/docs_cli.rs:438` for the same principle on `capabilities.md`.
- Every shell block declares `run`, `syntax` or `manual-live`. The gate syntax-checks all and executes only deterministic isolated examples with a temporary home. Manual-live examples never run under documentation automation and never contain a literal credential.
- A fresh product home is not a fresh machine. The verified fake turn proves local composition/session/agent/shutdown in this checkout; DOC02 remains active until the user-facing fresh-machine/provider examples have actual release/platform evidence. Diagram presence likewise is not enough: each SVG has a title/description/aria binding and passes both local and installed skill checks.

## 258. A declarative document is not activated until a product registry owns its exact row

- A host-neutral callback test proved dispatch but not product reachability. PL03 needed mutable token-owned registries for skills, commands, agent presets, hooks, themes and inference providers, then a real default-profile Consumer that resolves enabled immutable-cache rows and maps strict documents into those registries.
- Aggregation does not weaken disposal. Every adapter returns its exact registration token to the host-neutral bridge; partial failure and Context shutdown unwind only token-matching rows. Dynamic skills are snapshotted for prompt/tool/UI reads. Custom-agent preset instructions occupy the child system slot; the durable user message retains only the user task, while the strict request record captures the actual instruction context.
- Namespaced manifest names do not automatically satisfy each domain's identifier grammar. The product host derives one deterministic bounded kebab id with a hash suffix, retains the package/public name as provenance/display metadata, and lets the destination registry reject the vanishingly unlikely collision rather than truncating two rows into one.
- Bundled MCP must reuse the ordinary connection owner. PL04 freezes its JSON document and verified package root, canonicalizes a relative executable/cwd inside that root, checks executable type/mode and manifest permissions, builds a complete policy-bearing definition, then hands it to the existing stdio plugin. Teardown still kills transport before generated tools disappear; a parallel bespoke launcher would have forked lifecycle and approval semantics.
- Factory inspection cannot eagerly open a cache beneath an owner directory created by earlier plugins. The production factory captures only root plus validator; `product-extensions` opens/revalidates the cache during apply, after the credential-home boundary exists. The production-loader test seeds an admitted lifecycle row and proves registry publication plus withdrawal.

## 259. Background work becomes visible only after its durable settlement, and its UI must read the same owner

- A background operation needs its stable id and cancellation token before its future can run. `JobRegistry::spawn` reserves the row, then attaches the `JoinHandle`; failed attachment cancels, aborts and removes the reservation instead of leaving a running-looking row with no owner.
- Settlement has two phases. Reserving marks the row in-flight and may reserve one wake token; the Agent appends the exact inbox notice; only then does the registry publish the terminal state. A failed append clears `settling` and restores any reserved wake. Publishing `Settled` first would make UI state claim a result that replay cannot reconstruct.
- U22 does not maintain a second jobs/agents/diff model. Its three token-owned side-panel slots project the live JobRegistry/provider-preset registries and committed edit/write transcript facts into one bounded control-free snapshot used by both full TUI and screen-reader output. Ctrl+B is a persisted keymap action, not a renderer special case.

## 260. Installed delegated CLIs require their exact current session protocol, not a successful health probe extrapolated upward

- Codex executable admission must distinguish the lexical installed shim from its canonical script target. A sanitized PATH lookup verifies the selected lexical parent before canonicalization, then binds the interpreter/script identities; requiring the canonical target to remain under the PATH directory rejects legitimate Homebrew shims, while skipping the lexical check admits an attacker-controlled selector.
- Claude's current long-lived stream protocol is not the one-shot health query. A new stream may defer `system/init` until the first query, requires the official SDK control `initialize` frame, expects user content as an array with an empty session id for the initial query, and rejects an empty MCP object unless it is the exact `{"mcpServers":{}}` shape. The host must emit TurnStarted before the write so a fast response cannot overtake lifecycle state.
- Ephemeral means multiple independent controls: Codex receives an ephemeral thread start; Claude receives no-session-persistence, prompt-history suppression, a host-minted identity, strict empty MCP configuration and disabled Chrome/slash-command surfaces. Cancellation still calls the runtime operation and every outcome performs quiescent close. A tool-free installed canary proves compatibility/account use; deterministic event fixtures separately prove plan/tool/permission behavior.

## 261. A live workflow is evidence infrastructure, not live evidence

- QLIVE01 requires a process-scoped OpenRouter credential so the environment provider outranks this host's known dummy Keychain entry. Missing credentials write a credential-free Skipped artifact and fail the lane; they never become a green skip.
- The lane forces a live catalog, checks the exact GLM reasoning/tool facts, sends text and exactly-once tool turns through production composition, verifies the durable request headers and writes only closed content-withheld metadata. Freshness rejects future as well as expired timestamps.
- Checking in a scheduled workflow does not satisfy POR04 or QLIVE01. Only a fresh Passed artifact from the authenticated route does. Public catalog metadata can update deterministic descriptors, but it cannot prove that the selected account/model accepted reasoning and tool requests.

## 262. A signed release must bind the verifier process and the exact source tag

- A host-supplied verifier trait is not product reachability. `release-manager-gh` now injects the ordinary subprocess service, resolves the official GitHub CLI through that policy, proves `attestation verify` exists with one joined bounded help probe, clears inheritance, supplies only isolated HOME/cache/config variables, bounds deadline/output, joins every worker and makes its lifecycle token a Context effect. The verifier's stdout/stderr, temporary paths, subject and bundle bytes never enter public diagnostics.
- Repository and workflow identity alone still admit the correct workflow from the wrong ref. Production release trust therefore requires an explicit normalized `refs/tags/...` source identity and passes it as `--source-ref` alongside repository, signer workflow, OIDC issuer and the offline bundle. The workflow itself refuses to sign unless its dispatch ref equals `refs/tags/v<version>`.
- Update policy is operation-time state. The release CLI snapshots every enabled installed manifest's API range immediately before apply or rollback, authenticates the manifest before channel/API admission, then verifies the artifact and mutates. Fresh installs obey stable/preview/pinned policy too; absence of a current version is not permission to install a prerelease on Stable.
- Deterministic fixtures prove the service/CLI transaction, not the public trust root or a heycode release. Q14/Q15 remain active until a real heycode workflow artifact and bundle pass the shipping command. Q16 remains separately gated by three native real-provider turns.

## 263. A release matrix must execute the installed native artifact and parse its evidence

- A platform-neutral literal `bin/heycode` silently produces the wrong stable path on Windows. `InstallRoot::current_binary` now uses `heycode.exe` under Windows and `heycode` elsewhere; the release workflow feeds that exact release-manager-installed path into both onboarding smokes. Copying the downloaded candidate directly would test the artifact while bypassing the installer being certified.
- Q16 needs two different observations. The deterministic fake turn proves packaging/startup without provider variance; a second isolated home performs the credentialed GLM turn. The scripts never echo the credential or provider output on failure and their artifacts retain only platform, non-zero run id, fixed check names and `deterministic_fake|real_provider`.
- JSON existing is not evidence admission. Strict schema-v1 parsing bounds bytes, rejects unknown/future/incomplete/duplicate rows and maps only the certified platform tokens. The final workflow job downloads all three content-free real artifacts and runs `heycode release evidence`; only the evaluated macOS+Linux+Windows matrix can succeed.

## 264. Cloud endpoint capability, model capability, and product reachability are three gates

- Bedrock Mantle exposing Responses or Messages does not prove every listed model accepts that API. Each provider wrapper now rejects known incompatible model families before transport, and invalid default profile metadata fails construction. The selected model—not a provider-wide cache bit—owns cache placement/count/TTL evidence.
- Operation credentials and cancellation remain caller-owned across provider wrappers. Converse and both Mantle profiles retain only the exact credential reference, resolve once per operation, observe rotation on the next operation and thread the caller token into transport; a wrapper-created token would sever cancellation while looking locally correct.
- Provider-local request metadata is not shared-wire support. Cache points, guardrails, cross-region targets and reasoning state stay active gaps until the shared adapter serializes/parses them, session/usage preserves them, product factories expose mutually exclusive routes and a hosted canary passes. One crate's 130 green tests cannot promote PAWS04–06 across those missing owners.

## 265. Composing an installer must not create the installation it has not authenticated

- The first product manager eagerly called `InstallRoot::open`, which creates directories and the durable record. A missing or invalid manifest therefore left installation state even though no release proof had passed. Service construction is now read-only: an absent root projects empty state, manifest signature and policy run first, artifact verification runs next, and only then may `InstallRoot::open` publish the fresh root.
- Moving `InstallRoot::open` was not sufficient while verifier scratch lived at `<install>/.verification`: plugin construction still created the install directory. The CLI now owns an ephemeral OS temporary root whose lifetime encloses composition and verification; verifier setup and a missing/invalid bundle leave the requested install path absent.
- Existing installs still need a state snapshot before update policy. The approval token binds that observed current version, and `InstallRoot::install` independently compares it again after verification, so another process changing current state cannot reuse a stale approval.
- Context closure is checked again before opening/mutating the root, and the verifier lifecycle token cancels its owned process. Once atomic filesystem commit begins it finishes or recovers through the transition journal; interrupting halfway would be less safe than completing the authenticated transaction.

## 266. Grounding provenance must not invent spans or turn secret references into values

- Vertex external-API grounding is distinct from Google Search. Provider-owned admission accepts no-auth or a Secret Manager resource reference; it has no slot for the external API-key value. Public `retrievedContext` URLs can become durable-ready citations, while proprietary query/snippet material remains provider-local.
- Citation anchoring is evidence, not string convenience. One unique excerpt may retain its exact span; a missing or repeated excerpt becomes an unanchored source rather than guessing the first occurrence. Duplicate text must not manufacture positional certainty.
- Code execution and cache use keep correlation and absence semantics separately: opaque call/result ids and closed failure classes remain exact; implicit/explicit cache provenance, absent versus reported zero, and modality detail do not collapse. Claude-on-Vertex thinking/effort/tool fixtures likewise remain provider-owned until a data-driven shared Messages dialect consumes them.
- PGCP05–07 therefore stay active after 117 provider tests. Shared capability vocabulary, option serialization/event emission, durable cache usage, factories/credentials and authenticated global-region runs are separate acceptance gates.

## 267. A code-plugin generation commits atomically, but an abstract host is not an execution sandbox

- PL09 now separates candidate handshake from generation publication. All six domain adapters must accept before proxy availability flips; refusal unwinds LIFO, and crash/cancel retires the whole generation before any admitted response can escape. A remote per-call denial is different: it must not erase a healthy process generation.
- Component identity is exact bytes plus WIT world and engine ABI evidence. PL10 rejects dylibs/core modules, pins the current WASI 0.3.1 imports and defaults to no preopens/endpoints/ambient environment. That contract is still a lower boundary until a real Component Model engine typechecks it and a capability-scoped host supplies only the minted grants.
- A curated inspector must be unable to leak by construction. Its projection has no body, path, locator, digest, runtime id, error, settings or credential-reference field. It remains unregistered while QSEC01 approval is absent; a safe data type is not authority to expose it to the model.
- Abstract process/engine tests do not complete PL09/PL10. heycode-exec must bind executed bytes, clear environment, scope filesystem/network, own framing/cancellation and reap the tree; root must provide six real registry adapters and managed package/grant/session ownership.

## 268. Transcript virtualization starts at event correlation, not at clipping the final string

- Replaying one open tool/provider slot makes interleaved durable history quadratic or wrong before rendering begins. U17's reducer correlates every call/request/output identity first, then produces bounded provider-state/server-tool/usage/citation/compaction/runtime/route/plan items shared by full and flat views.
- Rendering all items and clipping afterward is not virtualization. U21 caches item fragments under a hard cap and walks only the requested viewport; the 100K-event middle frame creates at most 40 new items and retains at most 256 fragments. Performance tests state explicit replay/render budgets and retain middle as well as bottom viewport evidence.
- Human commands need a plane decision per command. Diff, copy and mention use the private UI bridge and never become model history; review is deliberately `model_scheduling` and its exact review prompt is durably admitted. OSC52 is a bounded terminal capability, not arbitrary shell clipboard execution.
- Theme/keymap/Vim changes commit through Settings CAS before live publication. `keymap` and `ui-preferences` are ordinary contributed namespaces, and both full/flat renderers consume the committed generation rather than keeping a second preference state.

## 269. MCP human events and tool approval are action-time policy, not server annotations

- MCP11 routes elicitation, progress and logging by exact client/session correlation with bounded human-safe payloads, cancellation and panic containment. Finite buffered HTTP/SSE can prove parsing/routing, but it cannot answer an elicitation before the same response stream closes; that requires a genuinely duplex/streaming exchange and remains disclosed.
- MCP13 filters the generated tool set by exact server/tool allowlist and performs approval when the tool is invoked. `readOnlyHint`, idempotence and every other server annotation are structurally absent from the approval request, so advisory metadata cannot weaken host policy.
- Credential references stay operation-time bindings on `McpBoundServer`. Provider bundles specify exact query plus optional/Prompt/no-exposure policy and reuse the ordinary connection/generation owner; secret-free specs or unit host callbacks do not prove configured product activation.
- O09 handler ports retain provenance/failure policy, but prompt/subagent/MCP hook output cannot become model input until a root-owned durable event exists and the actual lifecycle decision point invokes the hook. Registering a handler is not invoking it.

## 270. A provider's broad catalog protocols must narrow to one exact dispatch protocol

- Provider descriptors may truthfully advertise multiple supported protocol families, but one selected inference adapter must expose exactly the wire it will dispatch. Returning the broad OpenAI descriptor from a Responses adapter made Agent request projection ambiguous even though catalog evidence was correct. Broad catalog identity and exact adapter identity are separate values.
- Hosted tools need both classification and an executable upper bridge. OpenAI web/file/code/hosted-shell now cross provider transport/state/replay; computer, image and approval-bearing MCP remain safely classified/gated but inactive until their host/media/approval owners exist. Static all-tool plans would bypass N01 prefer-local policy, so tool options must be selected per request.
- Anthropic server-tool settlement may occur in a later request and `pause_turn` must replay exact state. Aliases, MCP restrictions, deferred tool search, call/result/citation correlation and signed state remain provider facts rather than flattened text.
- OpenRouter aggregate search usage has two documented shapes and neither supplies individual search ids/results. Normalize real aggregate counts and citations, but never manufacture per-call rows. PDS04/POR05 remain live-evidence rows; fixture completeness cannot satisfy them.

## 271. Optional delegated runtimes publish absence, but live acceptance still needs the real child

- A missing optional agent CLI is not a composition failure and must not erase the runtime from discovery. Resolve and bind executable identity during plugin activation; if and only if the installation is genuinely absent, publish the immutable descriptor with `AccountStatus::Unavailable`. Permission denial, replacement, malformed version or protocol drift still fail loudly.
- Connection lifetime and caller-operation lifetime are separate tokens. A successful start must not leave the published child attached to a caller token that may be cancelled later; the plugin generation owns every connection token, while each operation receives an independent token and session `close` performs quiescent reap.
- Protocol fixtures, an installed version/catalog handshake and exact default composition prove different layers. R10 still needs an authenticated OpenCode GLM turn. R12 still needs a runnable local Harness SDK artifact and observed prompt lifecycle; a checkout without the built server cannot be promoted by substituting fixture evidence.

## 272. Automation publishes after the durable checkpoint, and optional Consumers do not rewrite exact profiles

- Background process state has two owners that must converge in order. The terminal/process waiter settles and reaps the tree; the Agent then appends the source-attributed inbox notice; only after that commit may the JobRegistry expose completion. A concurrent `/stop` must join the same waiter, not race a second kill owner or return while descendants remain.
- Automatic continuation needs an authoritative idle edge. `AgentIdle` fires only after the active cancellation lease has dropped, while the turn gate is still held; goals can checkpoint, reserve one bounded wake and enqueue without inferring liveness from UI spinner text. Workflow checkpoints and schedule dispatch use the same durable-before-work/publication rule.
- A schedule dispatch is enqueue first, correlated dispatch record second, then flush and wake. Recovery recognizes only that exact enqueue-without-dispatch window. Fork projection starts at the physical suffix so inherited timers do not duplicate; copying is an explicit new-id operation.
- New default plugins are not automatically a config migration. `execution-jobs`, `goals`, `workflows` and `schedules` are optional Consumers whose services no historical row injects. Profile-free startup derives the live defaults, while inserting them into an intentional exact profile would violate that profile's authority; schema v25 therefore remains current.

## 273. MCP conformance cells must say which client drove which authorization boundary

- “Inspector passed” and “OAuth passed” are independent observations unless the official Inspector itself completed the authorization flow. MCP15 runs Inspector 2.4.0 against real stdio and Streamable HTTP listings/rich calls, while heycode separately drives a stateful local authorization/resource server through resource→issuer discovery, state, S256 PKCE, audience, refresh, cancellation and body-free failure checks. Record both; do not merge them into a browser-OAuth claim.
- Semantic HTTPS validation and TLS transport evidence are also separate. The OAuth boundary rejects non-HTTPS metadata and the fixture uses HTTPS identities, but a custom loopback transport reaches the local server over plaintext because the shared HTTP layer has no test-CA injection seam. This is enough for the local protocol matrix, not evidence of production Reqwest TLS against a third-party authorization server.
- Finite buffered SSE can admit a server request, process later progress/log/cancel frames and then POST a reply. It still cannot service an SSE response that remains open waiting for that reply. MCP11 remains active until heycode-http exposes a genuinely streaming response owner that can branch JSON/SSE and permit concurrent reply POSTs.

## 274. A benchmark smoke, a statistical report and a product claim are three different outcomes

- Q10's debug CI ceilings are deliberately generous hang/regression guards. Passing startup, TTFT, replay, render and registry p95 locally does not certify release speed. Release ceilings stay `proposed` until reference hardware and an accepted content-free baseline are recorded; relative drift is then compared to that actual artifact, not to an invented number.
- Q11 must emit confidence evidence even when it cannot decide. Matched candidate/reference order, identical declared conditions, deterministic graders, Wilson intervals and paired bootstrap output are the product. Nine successful pairs correctly yield `insufficient_evidence` under a 30-pair floor; changing the floor or calling the tie a win would invalidate the suite.
- Black-box security denial is vacuous without a positive control. QSEC05 requires the hostile source to reach a genuine write request, deny mode to settle it without mutation, and a separate auto-approval run to create the isolated marker. Missing source wrapping is a product failure, not something the harness may ignore to report a green guard.

## 275. Computing a security wrapper is not enforcement if one sink copies the pre-wrapper value

- Strict request projection correctly derived `content = boundary.render_for_model(raw)`, but the Tool match arm passed `message.content` into `ChatMessage::tool_result`. System/User/Assistant paths consumed the derived value, legacy compatibility mapping did too, and durable provenance remained present—so ordinary unit coverage looked healthy while the strict product route silently dropped every Web/MCP/LSP warning.
- Fix the shared sink, not each source. Passing the derived `content` closes plain and rich tool-result paths for all current and future `UntrustedContentBoundary` sources. A focused regression must enumerate every closed source and a no-boundary control must retain exact legacy bytes, call id and error state.
- The strongest proof crosses the whole product: the provider observes source label plus hostile canary inside a larger wrapper; it issues a real mutation request; deny policy durably refuses it and leaves no marker; the auto-approval control creates the marker; both process and fixture settle. Any missing premise is a failed evaluation, not a safe result.

## 276. Markdown parsers style text; they do not make terminal controls safe

- pulldown-cmark and syntect preserve text payloads. An assistant string containing ESC colour bytes or OSC 8 hyperlink bytes reached ratatui cells unchanged even though the flat/screen-reader renderer had its own sanitizer. Escaping terminal chrome is unrelated to sanitizing model text inside the frame.
- Sanitize before Markdown parsing/highlighting, at the shared assistant-text boundary. Preserve logical newlines (normalize CRLF/CR to LF) and visible text, turn tabs into spaces, and replace every other `char::is_control` value. Sanitizing only the final byte writer would mix application chrome with content and make styled-span tests unable to prove the cell invariant.
- Keep both a focused unit regression and the original corpus seed. The public draw-path fuzz target renders twice into a fixed TestBackend and requires every cell control-free; the complete local smoke then exercises session/config/provider/MCP parsers plus the render seed. Short runs close the defect and Q13's deterministic framework, but not Q12's unobserved scheduled continuous-run acceptance.

## 277. A child-process app-server gives stdout to the protocol and defaults trust explicitly

- The shipping IDE host is a real composition mode, not a wrapper around a TUI. `app-server --stdio-v1` parses a closed subcommand, canonicalizes an absolute workspace, admits only canonical UUID resume ids before joining the sessions root, composes the ordinary world, and hands stdout exclusively to framed JSON. Even a normal Agent UI delta listener would corrupt the wire.
- An editor launch is explicit execution authority, not automatic project-content trust. With no trust flag, the noninteractive host chooses `RestrictedOnce`; a user may place `--trust-workspace` before the subcommand after reviewing project config/skills/executables. EOF or Ctrl+C cancels/settles the transport and Context still unwinds LIFO.
- Installed proof must use the packaged VSIX and shipping binary. Activation-only or Node fixture tests are insufficient. The accepted journey opens, sends, overlaps cancel, closes, exact-id resumes in a fresh process, sends again and disconnects; permission allow/deny remains fixture-covered when the selected provider emits no installed request.
- A finite scripted fake provider becoming unavailable after its sole response is not a session-lifecycle regression. Name the exhausted fixture premise before treating a second-turn result as evidence about AppServer or RuntimeSession reuse.

## 278. Provider options are selected-request facts, and Bedrock reasoning needs its own durable state kind

- Static provider options run too early for model- and N01-dependent behavior. The provider hook receives a `ProviderOptionContext` containing the exact selected model and already-resolved native routes; Agent must call it after selection and before P10/request-header commit. An Unsupported/Unproven result is a preparation failure, never permission to fall back to an all-request option.
- Google Search, external grounding, code execution and cache controls are different option families with different providers, native routes and model evidence. A configured capability absent from the selected N01 set stays out of both durable header and wire. External grounding is not renamed as Search just to fit an existing feature enum.
- Bedrock cache points, guardrails and route metadata can cross durable options and detailed usage without a new session kind. Opaque reasoning signatures, redacted content and ordered assistant blocks cannot: accepting them without `ProviderStateKind::BedrockConverseMessage` would make replay lossy. Fail body-free until core validates the complete state, session reopens/projects it, and Converse emits/replays it only for the exact route.
- Shared adapter tests do not make a cloud route reachable. Exact mutually exclusive factories, settings/policy ownership, operation-time credentials, N01 contributions and hosted canaries remain independent acceptance gates.

## 279. Duplex HTTP needs a pull-owned body, not a background drain pretending to stream

- A buffered `send()` cannot answer an MCP server request while the same SSE response waits for that answer. The provider-neutral HTTP seam must publish validated status/headers before body completion and let one caller pull bounded chunks under its cancellation token. Cumulative bounds and backpressure belong to that owner; a spawned drain task would detach lifecycle and turn slow consumers into memory growth or orphan work.
- Existing buffered transports can remain compatible by adapting their body into one chunk. Dynamic transports must keep body and header values out of Debug/errors, distinguish explicit EOF from failure and never yield bytes after cancellation.
- MCP owns concurrent work locally: parse the open SSE, admit each exact correlated server request, run the elicitation handler and reply POST in an owned `FuturesUnordered`, continue progress/log notifications, then consume the terminal response. Cancellation retires the pending id and emits no late reply. No handler task outlives the request future.
- Canonical executable/cwd and pinned Node/package versions make MiniMax/Z.AI launch plans safe, but unresolved operation credentials plus specs are still not a connected generation. Root must build `McpBoundServer`, register factories and observe real discovered tools before PMM04/PZA05 can complete.

## 280. A safe local-runtime canary stops when the installed catalog cannot name the reviewed model

- Version and executable hash binding, initialize and credential-blind catalog discovery are useful runtime evidence, but they do not authorize substituting a nearby model. OpenCode 1.18.21 passed those steps; because its catalog did not expose official id `opencode-go/glm-5.3-flash`, the R10 turn was correctly not sent. The former Ox pre-release name is historical provenance, not a current route fallback.
- Runtime canaries default to no tools and content-withheld observations. They may report route/model/status/settlement metadata, never token files, response content or ambient credential values. Absence of a requested catalog row is an honest skip, not a failure to bypass.
- Bind every executable boundary that can change after review. The Harness bridge now hashes the launcher plus explicitly registered reviewed artifacts; version text alone cannot prove a script tree stayed the same.
- Do not “repair” an upstream live fixture by building a materially dirty pinned checkout under the wrong package-manager version. The pinned Harness checkout lacks its built server/tsx, requires pnpm 11.7.0, has pnpm 9.15.0 installed and a modified lockfile. R12 remains externally blocked until a reviewed artifact exists. Likewise, absent Ollama executable/daemon means PLM05 has no local live evidence.

## 281. Team/review reachability can ship without inventing a mutable worktree base

- O07's team service is ordinary composition: durable `team/change`, root/child authority filtering, global/task CAS, acyclic dependencies, bounded mailbox waits and JobRegistry→A03 settlement order. A TUI panel is optional UX; the effect-owned service plus model `team` tool is the product Consumer and exact inventory must prove both.
- O14 review captures current tracked state at operation time through the composed Git subprocess owner, commits the full exact base/patch/instructions before dispatch, uses a delegated permission-capable runtime under DenyAll, rejects any worktree mutation and commits only strict structured findings. `/review-runtime` is a distinct selectable command; the existing `/review` active-route prompt does not silently change meaning.
- O06's reusable worktree subagent provider deliberately requires a validated full commit id at construction. Do not make it default by silently resolving mutable `HEAD` in the composition root or bypassing the composed subprocess boundary. Until config/settings supplies an explicit base and root registers runtime-specific factories, the library implementation is active substrate, not product completion.
- New closed `team/change` and `review/change` events are log-only for neutral provider/TUI projection unless a dedicated renderer consumes them. Every reader must still match them explicitly, v1 must reject them, redacted export must omit their bodies and generated event references must remain fresh.

## 282. A security-sensitive optional config field still needs a new schema version

- `subagent.worktree_base` changes executable behavior. Reusing schema 25 would let an older binary parse the document, ignore the unknown field and silently run without the requested isolation. Schema 26 makes older readers refuse before partial loading; v25→v26 preserves every plugin choice and simply leaves the new optional field absent.
- The value is a full lowercase SHA-1/SHA-256 `GitCommitId`, validated while the factory table is built—before plugin effects. Presence conditionally registers exact worktree Provider factories; absence registers none. Do not interpret a branch/ref/HEAD string or resolve a mutable default.
- Codex, Claude and OpenCode delegated runtimes prove permission callbacks and receive separate provider ids/storage generations over the same exact base. DeepSeek Harness does not and stays unregistered. Runtime absence may remain a truthful unavailable row, but capability mismatch fails activation.
- Product evidence needs a real Git repository and state root disjoint from it. Composition must show the three provider rows and Context disposal; the lower manager tests separately prove exact checkout, synchronous cleanup/retention and orphan recovery.

## 283. Contextual provider policy and continuation state cross different durable planes

- Model/N01-dependent policy belongs in `request_options_for(ProviderOptionContext)`, called after exact catalog and route selection but before P10/header commit. Static options would either enable every configured tool or omit the selected one; falling back after Unsupported/Unknown would turn missing evidence into permission.
- Bedrock cache points, guardrails and route facts are request options; read/write/TTL counts are neutral response metadata. Opaque reasoning signatures, redacted content and ordered tool blocks are continuation state. They require `BedrockConverseMessage`, complete union validation, exact-route session projection and verbatim replay—never a distilled signature field.
- A successful Bedrock stream buffers one complete assistant state and publishes it only with terminal metadata/usage/finish. Empty output emits no state; wrong provider/model/protocol/kind fails before transport. Tool-use state participates in the same immediate-result chronology as neutral assistant calls.
- Provider state often contains model text and opaque continuity material. `ProviderStateItem::Debug` must redact `data` for every protocol, not only Bedrock. Persistence/equality still see exact JSON.
- Product-owned Google Developer composition may publish the shared registry/selection first and let `inference-google-gemini` register the exact provider later. Only the maintained default is admitted with an explicit empty native policy; other models/Vertex features remain unselectable rather than inheriting evidence.

## 284. Exact plugin bytes must outlive exec, and a WASI store owns its own limits

- Copying verified plugin bytes to a private executable and deleting them as soon as `spawn` returns is a TOCTOU race: the child may not have completed `exec`. The raw process handle must retain the exact-image lease through terminal settlement; cancellation/kill/reap then releases it.
- PL09's process launcher uses an empty environment, common sandbox/process-tree owner, bounded length-prefixed framing, serialized correlation and one atomic six-domain retirement gate. A real process test must fragment frames, cancel mid-call and prove descendant reap; abstract launcher fixtures are insufficient.
- PL10 compiles/typechecks a real Component Model binary against the pinned WIT import subset. Wasmtime 48.0.1/WASIp3 starts with empty-default WASI, admits only explicit preopens/IP endpoints, refuses DNS and write-only preopens, and applies fuel/epoch/table/memory bounds per Store rather than through global mutable state.
- A real engine and lower host still do not authorize installed code. Root needs an owner for package entrypoint/runtime selection, grants/resources/session and six production adapters. PL09/PL10 remain active until that product generation is reachable; PL11 remains approval-gated.

## 285. Experimental media must not widen every shipping protocol

- Audio groundwork is a distinct hidden adapter plane, not another `ChatMessage` variant. Exact provider/model/format evidence is required before association; every current provider inherits absence and therefore cannot advertise audio accidentally.
- Input bytes are re-read and hash-verified before `user/attachments` becomes model-visible. Provider output bytes commit to ATT01 first, then `assistant/audio` metadata commits, then UI/app-server publication. Cancellation before terminal success leaves no assistant audio row.
- Stable Rust/TypeScript/TUI surfaces carry only validated duration/rate/channel/depth metadata. Unknown `data`, `bytes` or base64 members fail decoding; terminal replay never smuggles samples through a notice string.

## 286. Async provider readiness belongs before the verified target, not inside streaming

- Some exact routes need caller-cancelled discovery after model/N01 selection: AWS needs private live callability evidence and Vertex needs the composed GCP profile. Blocking a runtime or resolving inside `stream` would either orphan work or dispatch a different endpoint than C02/C05 committed.
- `Provider::prepare_inference` is awaited and joined after catalog/model/native-route selection but before adapter borrowing, request options, P10 and durable request commit. Ordinary providers return immediately; lazy providers return an exact prepared operation provider.
- Catalog preparation and inference resolve credentials independently per operation. A stale public descriptor cannot unlock private AWS facts, and a maintained Vertex model row explicitly says nothing about account access.

## 287. A provider-native tool policy and its N01 rows are one generation

- Root must not duplicate provider tool ids or silently configure every classified family. OpenAI owns the explicit bridge-complete Search/code/shell set; Anthropic owns one exact atomic Search/code plan. Both policy types deliberately have no `Default`.
- The configurator and effect-owned candidate plugin derive from the same literal family set. Drift tests compare option materialization, inventory, registry resolution and LIFO withdrawal.
- Configuration-dependent file/MCP/advisor/search-index inputs and client-action/media loops stay excluded until their real owners exist. Safe classification is not activation authority.

## 288. DNS pinning is ineffective while ambient proxies can reroute the request

- Reqwest enables system proxies by default. A DNS-pinned URL can still travel through an ambient proxy unless the pinned client explicitly calls `no_proxy`; the proxy then chooses the destination and defeats the direct-resolution assumption.
- Public URL value types must reject obvious loopback/private/metadata/reserved-local targets before any provider-specific transport. Redirect and resolved-address checks remain separately necessary.
- Sandbox roots must fail on non-UTF-8 rather than lossy-rewrite authority. Windows lexical validation also rejects ADS colons, reserved device aliases (including extension/superscript forms), controls and trailing dot/space, while native junction/8.3 evidence remains a Windows-run requirement.
- A workflow naming `tests/it/foo.rs` as `--test foo` proves nothing when the crate uses one `tests/main.rs` binary. Dedicated jobs now invoke `--test main it::<module>`; YAML and cross-compilation still are not native runtime evidence.

## 289. A route-defining optional field still advances the config schema

- `llm.protocol` chooses Chat, Responses or Messages and can change auth/wire/state semantics. Schema 27 prevents an older reader from ignoring it and silently dispatching the established default. Historical documents migrate to explicit `auto` behavior.
- Bedrock Mantle has no safe implicit dialect: it requires `openai_responses` or `anthropic_messages`; Messages additionally requires an explicit positive `llm.max_output_tokens`. DeepSeek retains `auto`/`openai_chat` and makes `anthropic_messages` deliberate.
- Validation occurs before plugin effects. A protocol value incompatible with the selected provider fails loud rather than falling through to a nearby adapter.

## 290. Installed code authority cannot be inferred from a manifest request

- Manifest `[code]` may declare runtime and relative entrypoint, but the host separately mints provenance, unique session, exact capability grants and runtime resources. Missing authority fails; code packages may not downgrade into declarative-only activation.
- Six concrete adapters publish atomically behind one retirement gate. Native command/provider proxies own cancellation and the executable lease; Wasmtime reports the component's actual import subset rather than claiming the entire selected world.
- The default root uses the code-aware factory with an empty authority provider. This is product-reachable fail-closed behavior, not completion of managed grant/preopen/network configuration.

## 291. Release evidence must execute the candidate that was verified

- Running `cargo run` from the checkout after verifying a downloaded artifact tests the checkout, not the candidate. The candidate's own release command must install itself; onboarding must execute the exact stable path the manager published, without copying another binary over it.
- A real locally built heycode transaction can prove fresh install, update, channel/API refusal, directional rollback and fake turns. It must label fixture signature verification explicitly; it is not GitHub attestation evidence.
- Q14–Q16 remain active until a tagged hosted run verifies genuine attestations and native macOS/Linux/Windows fresh-machine provider turns.

## 292. Canonical disjointness checks must normalize absent destination paths too

- Reviewer worktrees must be outside the repository, but the destination may not exist yet. Comparing its lexical path with a canonical workspace misses aliases such as macOS `/var` versus `/private/var` and then fails later during plugin apply.
- Canonicalize the destination's existing parent, append the absent components, then compare. If the configured state root is inside the workspace, derive a workspace-id-scoped sibling root rather than blocking the entire default product.
- Keep diagnostics separated: unavailable repository, non-Unicode root, storage-inside-repository and repository-inside-storage are different configuration faults. A single combined error hides the corrective action.

## 293. Session-scoped product routing must know the durable id before plugins apply

- A fresh session that mints its id inside `session.apply()` is too late for MCP: every configured connection needs its exact product-session route before the factory graph is built. Mint one opaque id at the composition boundary, pass it to create-new/no-clobber session storage and derive every MCP router from that same value. Resume derives the already-durable directory identity and still lets `Session::open` perform the authoritative path/log validation.
- MCP approval and ordinary tool approval must share one policy object. Interactive policy construction before Context exists therefore needs activation-time event-bus binding; a second `Ask` instance would display and answer one waiter while MCP blocked on another.
- The product attachment is an effect plugin after hooks/session/Agent/subagents and before TUI. Hook output appends, flushes and reopens `hook/contribution` before it can enter model projection; TUI deactivation cancels elicitation waiters, and the connection owner emits no late reply.

## 294. Settings-derived provider inventory resolves during activation, not from a second file read

- Provider policy factories need the registered Settings namespace, but ordinary plugin factories are constructed before `settings.apply()`. Preloading the file into a second temporary service creates a TOCTOU generation: static inventory can describe one document while the live provider uses another.
- A Settings-backed provider wrapper retains fixed provider/catalog rows, resolves the authoritative snapshot during its activation transaction and dynamically contributes only configuration-dependent native rows. OpenAI/Anthropic provider options and their N01 candidates likewise derive from the same policy snapshot.
- New mandatory policy owners change exact-profile dependencies even when their fields default disabled. Schema 28 inserts only the selected OpenAI/Anthropic or cloud policy plugin; acronym-bearing enum spellings are explicit (`openai_responses`), with the accidentally derived historical spelling accepted only as an alias.

## 295. A managed authority fingerprint is not the admitted generation it names

- Profile v3 can describe exact code authority and carry a PL08 fingerprint, but a digest string cannot reconstruct the admitted source/catalog/host/policy objects or prove their bytes still exist. Converting it directly into `ManagedPluginAdmissionGeneration` would turn a reference into authority.
- Until a production managed-generation source is composed, root keeps the code-aware host empty and fails loudly when a profile requests code authority. Lower native/WASI tests remain real execution evidence, but PL09/PL10 stay active rather than receiving fabricated product reachability.

## 296. Build cache, installed binary and download bytes are three different size claims

- The ordinary full release measured 63,961,264 bytes while its reusable release cache occupied 1.6 GiB. The size-tuned full `dist` binary measured 18,609,680 bytes while its isolated cache still occupied 1.4 GiB. Moving `target/` changes neither installed nor download size, and adding another persistent profile can make the cache larger even while its binary is smaller.
- Distribution therefore has its own ephemeral profile: size optimization, fat LTO, one codegen unit and symbol stripping. It explicitly retains `panic = "unwind"`; heycode catches plugin, listener, hook and disposer panics, so `abort` would buy bytes by deleting a product containment contract.
- Measure the exact staged subject before attesting it. The local macOS-aarch64 candidate completed the fake product turn and compressed to 10,529,053 gzip-9 bytes or 9,096,990 zstd-19 bytes, but those transport sizes do not change the 18,609,680 installed bytes.
- A platform budget needs a platform observation. The workflow enforces 20 MiB only for the observed macOS-aarch64 artifact and emits `null` for unbaselined targets; invented Linux/Windows ceilings would be another form of “the gate exists” overclaim. A checked-in size step is still definition-only until its hosted jobs run.
- Compile distribution artifacts in an isolated `CARGO_TARGET_DIR` and discard only that explicit directory. Reusing or cleaning the shared tree reintroduces GOTCHAS #163/#168; an optimization whose verification destroys everyone else's cache is not an end-to-end improvement.

## 297. Detailed usage is an optional cost enrichment, not a protocol promise

- `input_tokens_details.cache_write_tokens` is GPT-5.6-and-later on `api.openai.com` and is absent on older models, Azure OpenAI and OpenAI-compatible gateways. `total_tokens` is present on every real frame and is therefore NOT evidence that a full detailed-usage object was sent; treating its presence as a gate is how a missing enrichment became a hard parse error.
- `responses_metadata` requires a counter only when the negotiated `NativeFeature::PromptCache` promised it or the payload actually carries it. Any residual inconsistency on a non-cache call costs the `ResponseMetadata` event, never the turn.
- The blast radius is why. The parse runs before `InferenceEvent::Usage`/`Finish`, so an error there kills the whole turn after text has already streamed — and retry is barred by `output_emitted`. An optional enrichment must never sit on the critical path of a settled turn.
- Every codec optional-enrichment parse must be proven to DEGRADE on a captured real provider frame, not only proven to succeed on a hand-built maximal one. The repo fixtures hid this for months because every non-detailed Responses fixture omitted `total_tokens`, a shape no real server sends. A fixture that no server would emit proves nothing about servers.

## 298. A model-bound capability gate in a schema-DEFAULTS validator aborts composition for everyone

- `native-openai` and `native-anthropic` sit unconditionally in `BUILTIN_PLUGIN_ORDER` and are handed the raw `cfg.llm.model`. Their Settings schema-defaults validator runs the model-bound capability gate at `settings.register()` time — i.e. on the all-disabled default value. Before the fix, any model id outside `gpt-5.6-sol` / `claude-opus-5` / `claude-opus-4-8` therefore aborted composition of the entire CLI.
- The zero-configuration baseline is now filtered by `hosted_tool_support` / `server_tool_support` per family, so an unproven model yields an empty executable plan; only explicitly enabled file-search/remote-MCP (OpenAI) or advisor/tool-search/MCP (Anthropic) still refuse. Absent is the right shape for "cannot prove", not a refusal (principle #15).
- Any future model-bound check placed in a schema-defaults validator must be safe for the defaults of every configurable model. The defaults are validated for a configuration nobody wrote.
- The suite stayed green through the outage because every openai/anthropic real-composition test in `crates/heycode-cli/tests/it/composition.rs` pins the one model constant the capability table admits. A composition test with a non-flagship model id is the guard that was missing.
- (Done: `production_openai_route_composes_with_a_non_flagship_model_id` composes `gpt-5.6-mini-test-only` through the production OpenAI route and asserts strict inference stays advertised. It passes — the empty-plan fix holds — and now pins it.)

## 299. A runtime event's sequence number is per-SUBSCRIPTION framing, not session identity

- `RuntimeEventHub` renumbers every subscription from zero, which is what lets retention be bounded without breaking R02's contiguity requirement. Consumers must not compare a sequence taken from one subscription against events from another: once a session exceeds `RUNTIME_EVENT_HISTORY` events, a fresh subscription's live events restart below a stored baseline and would be silently skipped.
- The correct pattern is to count events consumed on the current subscription, or hold one subscription for the session's life. `heycode-app-server` and `crates/heycode-cli/src/acp.rs` both now take the second option — one subscription per session, every turn or prompt pumping that same stream, no cross-turn dedupe at all.
- This is strictly better than the pre-fix state, where crossing the same threshold killed the Claude/Codex session entirely rather than skipping events.

## 300. Bounded retention of an R02 event log cannot use a blind pop_front

- Dropping the oldest event breaks the window's validity as soon as it crosses a turn boundary or splits a tool call from its result. Eviction must be structure-aware (`crates/heycode-runtime/src/event_hub.rs::trim`).
- The ordering must be finest-grain-first: progress, then a settled tool call, then interaction, then a whole settled turn. Preferring whole settled turns first overshoots catastrophically — it collapsed a 3,076-event window to a single event.
- `crates/heycode-agent/src/native_runtime.rs` still does the blind `pop_front` behind its own hand-rolled hub and is the remaining outlier; adopting `RuntimeEventHub` there is a follow-up lane.
- (Correction: the outlier is gone — `native_runtime.rs` now holds `Arc<heycode_runtime::RuntimeEventHub>` like every other adapter, and no blind-`pop_front` hub remains anywhere outside FIFO queues and test fixtures. The lesson stands; the follow-up is done.)

## 301. Fixture mode names can be prefix-significant

- `crates/heycode-runtime-codex/tests/app_server.rs`'s fake app-server branches on `${MODE#discovery}` and `${MODE#primary}`. Its `initialized` branch only stays quiet for modes matching one of those prefixes; any other mode name makes the fixture emit `fixture/ready` plus an unsolicited `fixture/approval` server request, which the session driver correctly rejects as an unknown request and which then presents as a baffling `Unavailable` on the next send.
- New session-level fixture modes must therefore be named `primary-*`. A fixture whose behaviour keys off a string prefix needs that rule written down beside it, or the next author debugs the product for a fixture's naming convention.

## 302. heycode-tui transcript heights are a CONTRACT, not a hint

- `transcript::height_bound` must never return fewer rows than `render::render_transcript_item` produces; the viewport uses it to choose where to start drawing. Growing a transcript view means growing the shared budget constant next to it (`render::tool_view_height_bound`, `markdown::render_markdown_height_bound`).
- `transcript::tests::every_item_variant_renders_within_its_height_bound` fails if the two diverge. It is the only thing standing between a new view and a silently clipped or oscillating transcript.
- The scroll offset is counted in RENDERED rows, not indexed rows — exact for the newest 32 items, whole-item granularity beyond that. `height_bound` is deliberately loose, so any future code that converts an offset into an absolute line via the height index reintroduces the dead-offset/oscillation bug that was just removed.

## 303. Untrusted text reaching the alternate screen is sanitized at exactly one place

- `render::render_transcript_item` rewrites every span through `markdown::terminal_safe_span` after the match. Do not sanitize per-arm — a new arm then ships unsanitized — and do not bypass the wrapper by calling `render_transcript_item_raw`.
- One chokepoint is auditable; N per-arm calls are a coverage argument that decays with every new variant.

## 304. A TUI panel is opened only through claim_panel_surface

- `AppState::claim_panel_surface()` is the single opener. The renderer, the key router and the screen-reader projection each stop at the first open surface in their own ordered cascade, so two open surfaces mean the user is typing into something the frame never shows.
- Never hand-write a close-list in a new opener. The list is the bug: it is correct only until the next surface lands.

## 305. inherits_parent_tool_guards is the one switch that can disable plan mode for a route

- Plan-mode enforcement is default-deny in two places: the `seam/pre_tool` allow-list for this agent's own tools, and a `DelegationGate` on the subagent registry for children the seam cannot reach. `task` is on the allow-list, but that admission covers only the native child.
- `SubagentProvider::inherits_parent_tool_guards()` defaults to false and decides whether an ambient session-wide policy can reach a delegated child. A new provider that returns `true` without literally passing the parent's `Waterfall<PreToolDecision>` into the child silently disables plan mode for that route. Returning true is a claim about wiring, not an intention.

## 306. An unreachable query-path error variant needs a documented absence, not a wildcard

- `crates/heycode-session/src/query.rs::map_open_error` deliberately carries no arm for an `OpenError` variant the query path cannot reach, and documents why. `OpenError::AlreadyOpen` in particular must not fall through the `_ =>` wildcard once some path starts opening for writing: `InvalidSession` would report a perfectly valid busy session as broken.
- The rule is that the path which makes the variant reachable adds the `SessionQueryError::OpenSession` arm in the same change. A wildcard that is safe today is a mistranslation waiting for a caller.

## 307. Replay safety is the NARROWER of neutral resolution and protocol evidence

- Neutral `resolve_request` derives a bounded retry policy from the draft and withholds replay for provider-native features and provider-executed native tool routes. A protocol adapter then knows things resolution cannot see, so it narrows further with `ResolvedCall::with_retry_spec` — never widens.
- The shipped Anthropic and OpenAI routes take the narrower of the two. An Anthropic turn is replayable only when it carries no native feature, no provider-executed route, and none of the Messages protocol evidence (compaction/server-tool block, container, non-direct tool caller, selected server-tool plan). "Every Anthropic turn now retries transient pre-output failures" was never true and is corrected here.
- Any future provider route composing through `resolve_request` out-of-crate must do the same or it re-opens this class of defect. The clean home for the rule is a `narrow_retry_spec(RetrySpec)` on `ResolvedCall` taking the min of the two safeties; until heycode-llm adds it, `heycode-provider-anthropic` carries local `narrower_safety`/`replay_rank` helpers that must be deleted in that same change.

## 308. One unprojectable session is a damaged session, not a damaged store

- Store integrity stays fail-loud only for what is about the STORE: a symlinked log or an unreadable directory fails the listing. A corrupt line or an unterminated tail is damage to one log — a crash mid-append is the commonest damage a real store carries — and is an unreadable row like any other (revised 2026-09-02; the first version of this lesson kept those two fatal and one crash-truncated session disabled `--continue`, `/resume` and the picker for every session). A directory that is not named like a session or has no `session.jsonl` is not a session and is skipped, not fatal.
- A well-formed append-only JSONL log this build cannot project — outgrown bounds, a newer schema, a broken semantic postcondition, a duplicated seq — is one unreadable row. `SessionSummary::is_readable()` marks it, it carries no invented facts, it sorts last so `--continue` still lands on the newest openable session, and it still refuses loudly when it is itself the target of resume, fork, rename, archive or export. `delete` refuses outright while any unreadable row exists, because its descendant scan can no longer prove a complete lineage.
- Surfacing is only half done until a surface renders it. Such a row has `title: None`, `created_at_ms: None`, `last_activity_ms: None` and `event_count: 0` by construction, so a UI that does not branch on `is_readable()` renders it as an ordinary brand-new session. Do not synthesise any of those fields to make it look populated.

## 309. Intermediate assistant text is commentary; exactly one final closes a turn

- R02 makes `FinalMessage` the terminal phase of a turn: anything after it is `PhaseAfterFinal`, and `TurnFinished{Stop}` without one is `MissingFinalMessage`. Mapping every `assistant/message` to `FinalMessage` therefore killed the native turn the moment a model said one sentence before calling a tool — the normal shape of every real turn — and an empty reply killed it too. The `--fake` demo died on its second message this way.
- Translation must be stateful: hold the message, drop or downgrade it to `CommentaryDelta` when a `tool/call` follows, and emit the one final (empty is legal, `allow_empty = true`) right before `TurnFinished`.
- Publish through `RuntimeEventHub` so the producer, not a distant consumer, fails on a violation. Doing so immediately exposed a second latent violation: an approval for a tool with `{}` arguments produced an empty `PermissionRequested.detail`, which ACP (unnormalized) accepted and the app-server (normalized) refused — one source of "approval → app-server is unavailable".

## 310. A consumer that shares one subscription across turns must forget it on failure

- The app-server pumps every turn of a session through one normalized stream. Once that stream yielded an error it stayed errored, so every later turn failed instantly with `unavailable`: one bad turn poisoned the session for the life of the process.
- Reset the subscription on failure and, on the fresh one, skip the hub's replayed history up to the first `TurnStarted` whose id this session has never projected. Skipping by "N turns finished" is wrong — the cancelled turn's own `TurnFinished` may still arrive on the new stream.
- Cancel the runtime's in-flight work when the client side gives up; otherwise the agent keeps running behind an idle UI and the next input is told "queued".

## 311. Body-free is not cause-free

- `RuntimeError` messages are validated safe one-liners by construction (`try_new`), yet Codex mapped every one of its own static messages to bare `RuntimeError::unavailable()`, and the app-server collapsed five runtime classes to the fixed string "app-server is unavailable". A missing `codex` binary reached the user as that string and nothing else.
- Keep the class fixed and carry the runtime's own redacted message as `AppServerError::detail` (validated again on the way in; `error.data.detail` on the wire). Render one error row per turn: if the specific cause arrives after the generic settlement row, replace the row rather than stacking a second.

## 312. A palette that owns the query cannot run commands with arguments

- The first palette kept its own `query` string and made Enter *insert* `/id ` into the composer. Every character — including spaces and arguments — went into the query, so `/model deepseek-v4-pro`, `/title Fix bug` and the product's own `/init apply <token>` matched nothing and Enter did nothing; a bare command needed two Enters; and because ranking fell through to descriptions with edit distance ≤ 2, `/memory` preselected `/compact` and Enter ran it.
- The composer is the single source of truth. The palette derives its query from the first token and never holds text of its own; Enter executes what was typed; only an id match is preselectable; Esc keeps the text; the idle submit path resolves unknown/unavailable commands itself instead of deferring to the loop.
- Snapshot journeys pin the flat frames, so a change to what the composer shows while the palette is open is a deliberate frame change, not a regression to suppress.

## 313. "Fail loud at load" is about plugin wiring, not about what the user typed

- A `[mcp.servers]` entry with a mistyped command, a `heycode mcp add --url` that was not a URL, one malformed `SKILL.md` name, a session log truncated by a crash and a `HEYCODE_HOME` spelled with `..` all stopped heycode from starting — some of them on *every later launch*, with an error that named none of them. Each was the law's fail-loud rule applied to user data.
- The rule: an unsatisfied `inject`, a duplicate service key or a malformed *plugin* contract fails composition; a value a user typed or a file a user can edit degrades to a visible, explained row (`/mcp` `Failed`, `/skills` `skipped — …`, an unreadable session) and the product starts. Where the user needs the old behaviour they opt in (`required = true`).
- Validate at the moment of entry with the same validator the runtime would use (`StoredServer::new` runs `McpStreamableHttpTransport::new`), so a bad value is refused when it is typed, not discovered at the next launch.
- A recomposition the user asked for (`/resume`, `/fork`, `/profile`) that fails before its shell appears returns to the previous world with the reason on screen; the user asked to switch, not to quit.

## 314. The prompt registered two sections and called itself a coding agent

- `AGENTS.md` and `CLAUDE.md` were never read: the system prompt was identity plus environment, and the product's own `/init` wrote an `AGENTS.md` nothing consumed. Codex prepends `AGENTS.md`; Claude Code loads the `CLAUDE.md` hierarchy. A user who writes rules expects them followed.
- Instructions are project *input* and go through the same trust gate as skills: Unknown blocks, Restricted permits read-only, Trusted permits. Read only the trusted workspace root and the user's home — never a git root above the workspace, which nobody trusted.
- Show the loaded files in `/status` (`project instructions: AGENTS.md, CLAUDE.md`). A rule the model silently ignores and a rule that was never loaded look identical from the outside.

## 315. A policy that can never say yes hides the bugs behind it

- `heycode plugin install` failed every user with "managed plugin policy is unavailable" because the only admission required administrator authority for everything. The first install that got past it hit a serialization bug (`previous: None` → JSON `null` → TOML "unsupported unit type") that had never executed. A gate nobody passes is untested code, not safety.
- Admit what needs no authority — declarative packages with no `code` section — and keep the managed requirement for packages that ship code, with an error that says which case applies. A lookup that answers the policy question must not create the cache it is asking about.

## 316. An approval nobody can answer is a hang, and one asked twice is a bug

- `approval.mode = ask` composed the interactive dialog policy on every surface. In a headless `heycode run` nobody answers, so the first tool call parked the process forever with zero output — and `heycode setup` defaulted to `ask`. Who can answer is a property of the SURFACE (`ApprovalPrompter::{Interactive, Proxied, None}`), not of the config: with no prompter, `ask` becomes `UnpromptedDeny`, whose reason names the fix.
- The mode is optional in config so the surface default applies: interactive asks, headless auto-approves. Claude Code and Codex both ask in the terminal by default.
- One decision per call. The native runtime mirrors every `ApprovalRequested` onto its event stream as `approval-<id>` for SDK clients; the shell that already got the original must drop the mirror. The MCP call-time handler consulted the same policy the Agent's `admit_call` had just consulted — a `Prompt` admission means "the ordinary approval applies", never "ask again".
- The card shows the whole command (a truncated command is a command the user did not approve), offers "allow for this session" as a `SessionAllowRule` (tool + program for shell tools, so allowing `cargo …` does not allow `rm`), and says "waiting for approval" in the status line instead of a busy verb. `/permissions ask|auto|deny` swaps the policy live through `SwitchableApproval` behind the `approval-switch` service; the status plugin injects `approval` so it is ordered after the plugin that provides the switch.

## 317. A composer that cannot take a paste is not a composer

- Bracketed paste was never enabled, so a multi-line paste arrived as keystrokes: the first newline sent line one as a prompt and the rest became steers. FEATURES.md called multiline paste shipped. Enable `EnableBracketedPaste` with the alternate screen, give the composer an `Event::Paste` arm, and grow the input area with the draft — a one-row composer hides every line but the current one.
- Ctrl+C had no visible state: the first press did nothing the user could see and any later press quit instantly, losing the draft. Clear first, arm visibly, quit on the second press, disarm on any other key.
- Escape is "back" or "close" on every layered surface and "interrupt" only when nothing is on top; it exits the product only where there is nothing else to do (first-run Welcome).

## 318. Configuration must be predictable before it can be trusted

- `--model x` lost to a `/model` from a previous session: the flag was only the Settings *base*, and the persisted user layer sits above base. Flags are the top ephemeral layer (`SettingsLayer::Override`, below managed locks) and an in-session write drops them — otherwise `/model` would stop working after `--model`. Say what the flag displaced, once, on the transcript.
- A project `heycode.toml` used to *replace* the home config wholesale: one line `[llm] model = "x"` silently reset approval, tools and MCP to defaults, and its missing `schema_version` produced a "migration pending" line on every start. Overlay files layer key by key and are not migration subjects.
- Unknown keys were silently ignored everywhere and a type error printed only `invalid type: string, expected u64`. Warn with file + dotted key; fail with file + key + line in *that* file (the merged rendering's line numbers mean nothing to the user).
- `llm.base_url` reached the DeepSeek catalog and Ollama only. The startup credential probe, connect flows, the OpenRouter/OpenAI/Anthropic catalogs and DeepSeek/OpenRouter inference all went to the official host — a key issued for a gateway was sent to the vendor to be "validated". Every request the configured provider makes must be built from the same base URL, and a recording-mock test proves the probe URL.
- `/profile` recomposed a *new* session and the picker could not return to the built-in composition. A profile switch changes the world, not the conversation: restart with `--resume <current>`, offer a `built-in (no profile)` row, and mark the current one.

## 319. An isolated home is not isolated while it shares the login keychain
The namespaced-store remedy below is superseded by home-file-only storage in #326.

- `KeychainCredentialProvider::system()` used one service label, `heycode`, for every home. A test with `HEYCODE_HOME=/tmp/x` — and the whole `RealCompositionHarness` suite — read the developer's real `OPENROUTER_API_KEY` from the login keychain, and `heycode setup` under a scratch home wrote into it. Namespace the service by home path; keep the historical label only for `~/.heycode`.
- Setup stored a pasted key on shape alone and Ctrl+C left it behind; the next start then failed preflight on a typo the user never saw. Check the key live before storing, keep a list of what this run wrote, and delete it on any error.
- "credential validation failed: network" is not a broken key. Start with a visible banner; only `unauthorized`/`host`/`model` mean repair — and say which store holds the key (`source: keychain`) and what fixes it.

## 320. A headless run is a Unix tool, not a transcript

- `heycode run` printed `[done:stop tokens: …]` on **stdout** after the reply, so `heycode run … | pbcopy` copied the trailer too; tool calls were invisible; the session id was never printed; piped stdin was discarded; and the same `print!` subscriber was registered inside the TUI, where it wrote behind the alternate screen and duplicated every line in flat mode. Reply → stdout; everything else → stderr; machine formats on request; exit code carries the outcome; subscribe only where the output is meant to go.
- Reading a non-terminal stdin to EOF hangs when a parent leaves the pipe open. Read on a thread with a bounded wait and say what happened.

## 321. Feedback the user cannot see is not feedback

- The context meter read `N tok` forever on the app-server path because only the native agent supplied an estimate: fall back to the last exact prompt size, warn before the compaction threshold, and let `/context` budget against the configured window instead of "unknown".
- A replayed `edit` card showed `{"diff": …}` as a quoted blob because the committed result is text; the live card showed the diff. Parse committed JSON objects back into structure at one place (`tool_result_value`) and use it on both the replay and the runtime stream.
- `heycode mcp test` answered `unknown` for every server: nothing installed a probe. A health command that cannot fail is decoration. The live probe spawns/initializes with a bound; its first run exposed that the convenience `McpConnection::spawn` dropped its runtime inside async on failure — a missing binary panicked.
- Every TUI launch minted a session directory, so the picker filled with empty rows. Decide "unused" from the log (housekeeping only) and remove it after shutdown; a profile switch from an unused session starts fresh instead of resuming a log that was just removed.
- `heycode mcp …` and `heycode plugin …` demanded workspace trust although they read only the home; home-only commands run with the workspace treated as restricted.

## 322. Follow-ups are only finished when the thing they described is gone

- A denial that cannot carry a sentence is a wall: the model learns "no" and nothing else. The reason is the model-visible text, so it must be normalised and bounded like any other model input — and while its editor is open the card owns every key, or a typed `y` answers the question.
- `list` probing every server made opening `/mcp` (and completing a name) start N processes. Listing reads; probing is something a user asks for. Splitting `list`/`list_probed` and giving `McpProbe` a concurrent `probe_all` made the asking version fast enough to keep.
- Composition connected MCP servers one after another, so three slow servers held the shell blank for the sum of their startup budgets. Register the rows sequentially (they need `ctx`), then drive every handshake together — and register disposal for the ones that started before reporting a required failure, or a rollback leaks children.
- An isolated `HEYCODE_HOME` writing to the login keychain means `rm -rf $HEYCODE_HOME` does not remove the secrets, and every test run leaves state on the developer's machine. Namespacing the service was half a fix; the whole one is that only the real home uses the keychain at all.
- `--screen-reader` writes zero bytes because a `dumb` terminal cannot take them — but a *requested* flat frame usually runs on a capable terminal, and refusing to enable bracketed paste there cost screen-reader users the same multi-line paste bug everyone else had fixed. What may be drawn and what may be asked of the input are two questions.
- A read-only allowlist of tool names beside the scheduler can never speak for a tool the crate has not heard of. `Tool::effect` moves the claim to the only place that can make it, and keeps deny-by-default for everything that stays silent.

## 323. A recorded policy nobody compares is documentation, not an invariant

- `RequestOptionsSnapshot.retry` was filled on every dispatch and rendered by `/usage`, yet `compare_header` never read it: only the safety rank bound ran, so changing `max_attempts` from 3 to 1 with identical safety verified clean. A policy difference with no footprint in `native_features`/`native_tool_routes` was unobservable by construction.
- The fix pins `retry_max_attempts` exactly whenever the durable row recorded it and keeps safety rank-bound. Safety needs the bound because an adapter narrows from protocol evidence the header does not carry (GOTCHAS #307); the budget has no narrowing rule, so drift is always a desync. Exact safety equality would reject safe narrowing — the wrong fix for the right gap.
- Legacy headers without the row skip the check instead of reading absence as a policy (`request.rs` already documents absent as "not recorded, never never"). A compare that fails old logs on upgrade turns a hardening into a migration incident.

## 324. A damaged row must say it is damaged on every surface that lists it
- `SessionSummary::is_readable()` existed and the store listed the corrupt log, but both TUI renderers ignored the flag: full mode showed `empty · unknown cwd` with 0 events and flat mode showed a bare id line. A damaged log rendered exactly like a brand-new session, so the user learned the truth only when resume refused.
- Branch at the renderer, not in the query: the store row stays factual (`title: None`, `event_count: 0`), full mode adds an error-colored `unreadable` marker plus an `unreadable` status in place of the misleading `empty`, flat mode appends `— unreadable`. One `bool` read per row, no new facts invented.
- Pin both surfaces in one test (`unreadable_row_is_visibly_damaged_in_full_and_flat_modes`): full via `TestBackend` frame text, flat via `ScreenReaderSnapshot`. A surface added later that lists sessions without branching re-opens this silently — grep `is_readable()` call sites when adding one.

## 325. An estimate and a measurement must not render alike

- The status-bar meter showed the native agent's chars/4 heuristic and the app-server fallback prompt size identically (`120 (12%) ctx`), although only the second is a provider-reported size. A rough estimate wearing exact clothes is the same defect class as C13's lower-bound percent (GOTCHAS #230), one layer up.
- Track provenance, not just value: `AppState.context_tokens_estimated` is set alongside the tokens in the `TurnFinished` arm (event estimate → true, usage fallback → false, neither → keep both), defaults to estimated so unknown provenance reads as estimate, and a `ContextMeter` struct carries it to both renderers. Estimates render `~120 (12%)` / `context: ~120 of 1000 (12 percent)`; exact fallback keeps its bare form.
- The existing fallback test name (`falls_back_to_exact_prompt_tokens`) is now load-bearing: it pins that the exact path renders WITHOUT `~`, while the estimate path asserts it. Do not "simplify" by marking everything estimated — that would unstate a true precision.
- Deliberately untouched: the warn threshold and never-regress-to-unknown rules behave identically for both provenances, and the detailed `/context` envelope surface already speaks exact/estimated/uncounted fluently.


## 326. A native secret-store probe is already an OS permission interaction

- A real TUI launch stalled before drawing and returned `credential provider keychain failed`. Namespacing non-default homes did not fix the default home: startup presence checks retrieved OS entries and setup could still persist outside the heycode directory.
- The product policy is now home-file persistence only. Remove the native provider crate and dependency, default row, factory, bootstrap/setup/doctor registrations and any OS migration or cleanup probe. Merely disabling writes or masking errors leaves a read path that can still prompt.
- Config v29 replaces old complete-profile keychain selections with one file-store row and preserves the original backup. Named/current profiles requesting the retired plugin fail with its replacement instruction. Old OS entries remain untouched; import would itself violate the no-probe contract.
- Repurpose native-store tests into file round-trip/lifecycle, missing/corrupt-store and distinct-home tests. A bounded subprocess with an isolated OS HOME and no HEYCODE_HOME override exercises the default-home path. A dependency regression rejects reintroducing native secret-store packages. Never mutate the test runner's environment or probe the developer's keychain.

- Verification: 3,931 workspace tests + 7 doctests, clippy/fmt, 263 focused CLI/config tests and five full-screen PTY scenarios pass. A compiled mutation keeping the keychain row was caught by the migration regression. Real-home startup reaches Welcome; the rejected OpenRouter file key remains a separate auth repair.

## 327. A subscription menu cannot repair an obsolete runtime pin

- The installed Codex 0.153.2 binary was rejected before account discovery because the adapter admitted only 0.146.0. Review the generated schema before changing the pin: this release adds eleven notifications and four account-plan variants. Keep the notification union closed and reject unreviewed releases.
- Successful credential-blind account/model discovery proves that discovery works, not that a delegated turn or the onboarding route works. Record these observations separately. A direct welcome selection also needs a regression proving that one confirmation selects the intended family.


## 328. Installed ACP agents may expose the legacy model contract

- Grok 1.0.13 returns `models.availableModels` and `models.currentModelId` from session creation. Preserve whether the catalog came from config options or legacy models: selection must call the matching `session/set_config_option` or `session/set_model` method. Validate bounds, duplicate ids and current-model membership at admission.
- Account discovery must consult the official process. Cached-token authentication is an official runtime operation; reading the vendor's token file into heycode would cross a different credential boundary. The installed tool-free Grok turn passed with explicit model selection.

## 329. Connection discovery and agent startup have different workspace authority

- Pre-trust version/account/model discovery must run in an effect-owned temporary directory, while an authorized session receives the actual invocation directory. Keep the temporary directory alive until all operations finish, including Context shutdown with a retained operation handle.
- Hiding the trust dialog behind onboarding does not gate runtime startup. Defer app-client open while either connection or trust is pending. A connection change must also discard old route/resume CLI arguments; credential repair retains those arguments. The two restart outcomes are deliberately distinct.
- API connection choices bind provider profiles to authorization descriptors by their credential reference, not a constructed flow-name suffix. Save the pending target without publishing it as the live Agent selection; activate it only after the new provider has composed successfully. Back from model selection returns to its originating connection page.

## 330. A welcome menu needs hierarchy and a reserved footer

- Bright bold titles identify every available action. Descriptions use indentation and secondary text; spacing groups each title with its description. Selection uses an accent background and arrow rather than dimming every other action.
- Reserve the footer before calculating visible rows. Scroll long catalogs around the selected row, and use compact rows on short terminals. Counting only option lines clipped the original keyboard footer.
- Routing persistence must omit absent optional fields: JSON null has no TOML representation. A real file-backed composition regression caught the failure that an in-memory Settings fixture could not.

## 331. Onboarding categories must match the way a connection is chosen

- Subscription, local model, API provider and managed cloud are distinct entry points. Cloud products own project/region/deployment forms rather than appearing in the API-key list, and only implemented cloud profiles may appear under that category. A local runtime cannot be routed into the API-key authorization-method picker.
- Reuse a saved valid connection on subsequent launches. Keep temporary network failure distinct from rejected authentication and preserve the stored route in both cases. A global unrelated API-key check must not decide subscription or local readiness.

### 332. Wizard filtering must drive selection, not just rendering

A list query belongs to onboarding state. Enter resolves against the filtered rows, and zero matches cannot select a hidden provider. Reset the query across page boundaries. Consume paste before it can reach the composer, bound search text, and render an explicit empty state.

### 333. Runtime recovery guidance belongs to the provider

A generic unavailable/unauthorized error cannot tell a person how to install or sign in. Carry bounded provider-owned help on the runtime descriptor so unavailable registrations preserve it and the TUI does not hard-code vendor commands. Display instructions after a failed check, without executing login or requesting tokens.

### 334. Compatible inference does not imply a compatible catalog

Fireworks inference uses a Chat Completions version root, but authoritative discovery uses paginated account-model metadata. Follow opaque pagination with URL encoding and fixed authority, bound pages/bytes/ids, and require ready serverless chat evidence for that product. Groq model listing alone does not prove tool support: use exact provider-documented models, leaving future ids unknown. Explicit proxy endpoints must keep credential validation and discovery on the selected authority.

### 335. A local connection need not have a model default

Keep connection metadata separate from an instantiated provider profile. A local library has no sensible invented fallback. LM Studio's shared inference picker projects loaded instance ids, while its native service retains downloaded and embedding records for management. Recheck the exact instance and tool/Chat evidence before durable request admission; choosing a model must not silently load another copy. Ollama's catalog must be registered before that provider is selected.

### 336. Saved intent and credential readiness are different states

A rejected or missing credential does not erase a saved route or make an existing user new. Resolve saved intent from the same settings snapshot as startup routing and offer targeted repair. A disconnected inference placeholder must not overwrite provider-owned authentication metadata with None. Inspect safe credential state before opening a masked prompt so already-configured connections can proceed to discovery.

### 337. Stale catalog success can carry rejected authorization

Inspect both refresh errors and successful stale views for Unauthorized before offering a model. A last-good model list does not repair a rejected key. Preserve the route and name its recovery target. A repair opened from `/connect` retains session-dismiss semantics; it must not turn Escape into quitting the application.

### 338. Draft endpoint discovery is a separate catalog operation

An edited endpoint must not inherit credentials or overwrite the active provider cache. Let the provider validate and discover the draft address through a caller-owned, registration-cancelled operation; only model selection stages endpoint and model together. Discard results if the input changed while discovery ran. Preserve the endpoint across model/runtime changes and clear unrelated credential bindings on recomposition. Optional route fields must be omitted from user TOML rather than serialized as JSON null.

### 339. Serverless lifecycle metadata belongs to the selected product

Fireworks model `deprecationDate` describes removal of its serverless deployment, even if the underlying model artifact remains READY. Preserve that date in the serverless catalog. A partial calendar date is not an exact timestamp; keep deprecation evidence without inventing one.

### 340. Model lists can mix incompatible task types

Mistral model cards expose chat eligibility separately from function calling and vision. Require explicit chat eligibility before offering a row in connection setup, exclude archived cards, and retain unknown for absent intrinsic capability evidence. A shared Chat transport does not prove every model-list row accepts Chat requests.

The same task-boundary rule applies to Together's native array and xAI's separate language-model endpoint. Do not send xAI discovery to the mixed `/models` list when an endpoint override is supplied. Preserve its `/language-models` suffix on the selected authority.

### 341. Endpoint keys belong to the pending connection

Do not inherit a key when the server address changes. Validate an explicitly entered key through that provider's draft catalog operation, persist it under a fresh reference, then stage reference/endpoint/model together. Failed validation cannot replace the previous key. The Settings schema must explicitly attest reference paths as public; naming a field credential_reference otherwise blocks wire-exposed schema activation. A one-operation authorization flow needs no permanent registry row, but must share the normal cancellation and authoritative readback transaction.

### 342. Local proxy credentials cover both native and inference protocols

Adding a key to an Ollama model list does not authenticate Chat: its historical placeholder must be replaced with the actual reference-bound credential. Native catalog requests may stamp a credential only onto the exact derived discovery URLs and must reject existing authorization headers; inference owns its own adapter binding. Keep raw transport available for unauthenticated draft discovery, and classify 401/403 independently from transport failures.

### 343. Terminal paint deltas are not complete screens

Stripping ANSI escapes from a ratatui update loses unchanged cells as well as cursor positioning. A Back transition may correctly retain letters from the previous frame, so its raw delta cannot prove the complete title is missing. A PTY assertion that needs the entire current title must reconstruct the terminal or request a full repaint through a real size change; keep the full-title assertion intact.

### 344. Discovery does not imply adapter admission

A model catalog can list more models than production composition accepts. Keep exact adapter coverage in provider-owned connection metadata, filter the picker with it, and enforce it again before staging a connection. Neither catalog membership nor an explicit tool capability proves that a composition factory can activate the model. The Gemini regression supplies a catalog-only model and requires rejection before admitting the maintained default.

### 345. Cloud coordinates must reach every provider consumer

Persisting a region or project is only half a connection change. Startup must restore the same coordinates into inference, catalog admission, credential validation and status without mutating process environment. AWS status names an explicit connection origin; it must not attribute a saved region to an environment variable. Native model/runtime changes retain coordinates, while an explicit different provider cannot inherit them. Coordinate settings accept bounded non-secret names; keys remain credential references.

### 346. Deferred work must disappear from product promises

Moving unfinished features to a backlog is not enough when welcome copy still names them. Remove their names, mock choices and teasers from reachable UI; preserve the implementation plan in documentation and an isolated task. Keep functioning integrations and their accurate prerequisites visible.

Setup help is production UI too. State the authentication and protocol the route actually implements; keep unsupported future authentication methods in limitation documentation instead of advertising them beside a working choice.

### 347. Draft cloud coordinates are not the composed provider configuration

A cloud catalog must mount before its coordinate form can be useful. Let draft-aware sources register with an unresolved host coordinate, make ordinary refresh fail honestly, and give the draft probe an exact provider-validated coordinate map. Never guess a region or publish a draft generation into the active cache.

An operation key must be validated against the draft coordinates and committed under the same reference later staged with the route. After restart, authorization, status, catalog admission and inference must consume that saved coordinate/reference tuple. A provider-default credential alias in any one of those consumers recreates the split-brain connection the atomic Settings record was meant to prevent.

### 348. Cloud authority is not one generic masked credential

Bedrock can validate a pasted provider API key for one draft region, while Vertex requires two different external facts: an ADC identity and a current `cloud-platform` OAuth token supplied through the configured credential provider. Ask the catalog whether draft credentials are supported before rendering a masked action; a shared coordinate form must not imply that every cloud account can be repaired by pasting a key.

Keep readiness content-free when the provider offers such an operation. Vertex `fetchPublisherModelConfig` is a bodyless GET for one exact project/location/publisher/model resource, so it can confirm the selected route without turning setup text into an unlogged model request. Validate the exact coordinate map first, preserve Absent/Faulted versus Undetermined ADC states, resolve the token per operation, classify the bounded response without rendering its body, and keep the resulting draft generation out of the ordinary catalog cache.

## 349. A shared protocol does not imply shared authentication or model identity

Azure OpenAI v1 uses the OpenAI Responses payload and stream shape, but an API key belongs in `api-key`, not bearer `Authorization`, and the request `model` is the configured deployment name. Parameterize that narrow wire difference in the shared adapter, then wrap it with provider-owned resource/deployment types and reject another model before dispatch. Forking the parser would duplicate replay behavior; treating Azure as a generic compatible endpoint would lose the authority and deployment invariants.

The setup-safe catalog and selected inference route are separate reachability claims. Mount readiness with unresolved coordinates, validate exactly `resource` plus `deployment` for a draft bodyless model GET, keep every unproven capability Unknown, and conditionally register inference only after the same persisted coordinate/model/reference tuple is complete. Microsoft Entra ID remains absent until an owned refresh-capable identity provider exists; a short-lived bearer token is not an API-key substitute.

## 350. A custom compatible server is an exact route, not a capability assertion

- Treat the supplied URL as a validated credential-free version root and append only the protocol paths the product actually owns. A query, fragment or userinfo can hide routing or secrets; protocol detection would turn one reviewed contract into an open-ended promise.
- Canonical `GET /models` proves model identity only. Validate the whole bounded generation and leave tools, reasoning, modalities, structured output and lifecycle Unknown. An explicit model id is a selection escape hatch when discovery is unavailable, not evidence about that model.
- Optional authentication has three distinct states: no binding sends no header, a masked draft key applies only to that draft endpoint, and a persisted reference resolves per operation. URL, model and optional reference commit together; once selected, a missing optional reference is a reconnect condition rather than permission to fall back to no auth. Setup-safe catalog composition performs no request, and inference exists only for a complete saved route.
- Do not add start, stop, load, pull or install controls to a generic server. There is no common reviewed management API behind OpenAI Chat compatibility.

### 351. A new provider family does not authorize a new welcome choice

Cloud metadata may remain a distinct provider-owned category while the product keeps its agreed three entry points. Route cloud and API profiles through the same provider picker. Preserve the three-row welcome invariant and test cloud forms through provider search, rather than rewriting the product contract to fit an implementation.


### 352. A selected endpoint needs tool admission without fabricated evidence

A bodyless model listing cannot prove function-tool support. If Azure deployments and custom OpenAI endpoints retain Unknown and use the default strict tools gate, every normal Agent request fails before HTTP because the Agent supplies its tool catalog. Tool-free adapter fixtures miss this product failure.

Keep identity discovery and capability evidence unchanged. These two provider constructors explicitly opt into attempting unknown function tools through the shared resolver; all ordinary routes default to evidence-required. Known Unsupported, unknown non-tool capabilities, malformed schemas and exact-route mismatches still fail before dispatch. The actual tools and tool results use the existing durable request/session projection, with no hidden probe, fallback, protocol switch or feature removal. A successful request does not rewrite the catalog to Supported. Verify a real composed Agent read-tool round trip and the logged second request, not just a transport fixture with an empty tool list.

### 353. Inline terminal surfaces must budget rows before painting

A picker drawn over a centred rectangle hides conversation context and makes
each capability invent a different chrome contract. Allocate header,
transcript, inline surface, composer and the two footer rows in one top-level
layout; ordinary command/model/provider/settings/permission/profile/session/
plugin/MCP/capability browsers receive only their reserved surface and use a
single top divider. Keep trust, secret, onboarding and approval decisions as
true foreground dialogs because they intentionally suspend the ordinary shell.

Terminal height is a priority system, not a scale factor. Preserve at least one
transcript row, collapse the command-hint footer before security-bearing
transcript content, and degrade a full header to one row before removing its
identity. When the header is one row, retain the active backend/model before
optional workspace detail; a stale native runtime label must not outrank a
delegated runtime that owns the live turn.

Shell chrome is ordinary typed Settings state. Add every header/footer field to
the schema defaults, validator, full CAS replacement and screen-reader
projection together. Theme and Vim shortcut writes must start from the current
snapshot so changing either cannot reset shell choices. A Settings watcher may
queue the validated shell snapshot onto the human UI bridge; the renderer must
never read Settings or persistence directly.

### 354. A model picker must retain who owns the active loop

The persisted native provider/model remains a useful fallback while a delegated runtime owns the session, but it is not the active model control plane. Projecting that provider into `/model` lets a Claude or Codex session show and persist a native API model the delegated backend never receives. Carry a typed native-provider or delegated-runtime owner plus the routing revision through command emission, async discovery and selection; recheck both immediately before mutation. Native effort values must come from the exact adapter/model metadata used by resolution, not a global low/medium/high list. Cancellation, an ownership change or a same-owner configuration change makes an old result stale, and a stale or unsupported submission changes neither Settings nor live Agent state.

### 355. Backend acknowledgement and durable configuration need separate outcomes

Record an attempted configuration before the child receives model-visible
inputs, then record committed or failed after settlement. Replay must not treat
an attempted or rejected value as effective. Failed initial launches cannot
consume fresh-session eligibility merely by writing their attempt record.
Validate the target session identity before changing its configuration, and
close a child if its acknowledged controls cannot be durably committed.

### 356. A logged tool call is not an execution reservation

The delegated event pump can precommit a tool call before its execution callback
arrives. Accept that exact first callback, but reserve its turn/call correlation
before approval or execution and reject a duplicate while it is in flight.
A durable terminal result also forbids another execution of that correlation.
Provider retries must not repeat a filesystem mutation just because their
name and arguments match an already logged call.

### 357. Inline picker height must follow the visible content

A fixed reserved surface leaves short lists floating above the composer even
when the surface itself is correctly anchored. Derive its height from heading,
rows and controls, cap it to the available viewport, and scroll to preserve the
selected row. Test both a short list in a tall terminal and a long list in a
short terminal; either alone misses the other failure.

### 358. Preflight cannot reserve an external Settings revision across an await

A delegated backend can accept model/effort while another writer advances the
routing revision. If the subsequent CAS fails, the backend must not continue
with values the saved route does not reflect. Carry the exact backend generation
with the acknowledgement and retire it on persistence or effective-response
validation failure. Compare runtime and generation during cleanup so a delayed
failure cannot close a replacement session. Reopen from the durable tuple.


### Custom-agent audit: policy belongs to the child execution registry

Filtering schemas alone cannot stop a provider from calling a hidden tool, and
checking only Agent admission leaves CodeMode or another guarded dispatcher
able to bypass argument-level restrictions. Bind a filtered registry whose
wrappers retain effect/rich-output/provenance behavior and enforce the same
policy on run and run_output. Reuse the actual parent's resolved registry,
including worktree filesystem/shell rebinding, before attaching child-specific
memory. Never retain a memory tool that captures another preset's storage.

Reload is a generation transaction: validate every agent first, check foreign
ownership and duplicates under the registry lock, then replace exact tokens.
Existing children hold immutable resolved policy. Scope aliases preserve their
source identity, so invoking an alias cannot split persistent memory. Imports
must reject semantically unsupported fields; a schema that accepts an ignored
permission is an execution defect, not compatibility.

## Native child admission, inbox turns and worktree results

- A background job may still be queued when its caller receives an ID. A cancellation test that asserts a provider observed cancellation must first prove the provider entered; a separate pre-start test proves no inference occurs when cancellation wins admission.
- Native task IDs are allocated before sessions/handles exist. UI and orchestration must read TaskSnapshot and observe the actual child Agent before its first send, rather than infer lifecycle from handle presence or parent tool cards.
- Tokio task-local parent identity does not cross `spawn`. Capture the actual parent at task admission, then scope both Agent and authority for every first, follow-up and mailbox/inbox turn. Otherwise a grandchild silently inherits the root model, workspace or owner.
- A native steer is durable next-step input, not interrupt plus replayed text. Reserve its owned job before append; consume by exact ID under the turn gate. Busy settlement can consume it first, so a missing claimed ID is successful deduplication, never authorization to consume a different message.
- Worktree cleanup must inspect tracked, untracked, ignored and committed divergence on every terminal outcome and recovery. `RemoveAlways` is not permission to destroy the only copy of child edits. Child model tools keep their inherited sandbox mode rebound to the lease; structural host Git operations are not model shell capabilities.
- Aggregate native request permits cover inference only. Holding them while a parent awaits nested tool work can deadlock the entire tree. Persist reservations before dispatch and never replenish the counter merely because a turn or process ended.
- An adapter without an account-query API has Unknown readiness. Unsupported introspection must not be mistaken for disconnected authentication or proof that its normal startup handshake cannot work.
