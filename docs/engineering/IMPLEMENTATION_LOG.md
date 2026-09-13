# Ship-ready implementation log

This is the durable handoff ledger for completed implementation slices. [TASKS.md](TASKS.md) is authoritative for status and dependencies; this file records why the code has its current shape, the tests that prove it and the clean next boundary. Append one entry after each completed feature—never rewrite history to make an old decision look inevitable.

## 2026-08-24 — M0-001: composition audit and config v1 migration

Tracker: `B01`–`B06` complete.

Outcome:

- `Context::plugins()` records successfully applied plugins in order, after each `apply` commit point.
- The default real composition is pinned at 13 plugins, 13 service keys/owners, 9 slash commands, 15 tools and one selected fake provider in deterministic tests.
- Persisted TOML has root `schema_version = 1`; unversioned, older, current and newer documents classify explicitly.
- The exact unversioned eight-plugin profile written by the old setup wizard produces a semantic preview naming `agent-options`, `skills`, `mcp`, `subagent`, `plan` and `sandbox` as restored rows.
- Startup auto-applies that fingerprint only when discovery selected the home config. It canonicalizes the target, compares the previewed source bytes, creates a byte-exact stable backup, rechecks the source and commits through a cross-platform atomic writer.
- Applying the same plan twice is a no-op. Newer schemas, changed sources and conflicting backups fail loud.
- Non-historical custom profiles keep their list, comments and unknown sections. Explicit/project documents are never auto-written; startup reports their migration as pending.
- Setup now emits the current schema marker and continues to omit `[profile]`, so it cannot create another capability freeze.

Architecture decisions:

1. Historical fingerprints live in `heycode-config`; the current built-in order remains owned by the CLI composition root and is passed into migration planning. This preserves crate direction and one source of truth.
2. Migration plans expose only semantic changes. Original and rendered bytes remain private so diagnostics cannot accidentally grow a secret-bearing raw-config surface.
3. Provenance participates in authorization to write. An identical project profile may be intentional, while the old wizard is known to have written the home file.
4. The richer settings/profile UI must consume these plan/outcome types rather than fork migration logic.

TDD evidence:

- Red: the kernel audit test failed to compile because `Context` had no applied-plugin inventory.
- Red: the real binary left the generated profile unchanged before startup wiring.
- Green: version classification, generated fingerprint preview, backup/atomic/idempotent apply, conflict refusal, custom preservation, explicit-source non-mutation, setup rendering, exact composition inventory and two real-binary startup paths.

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 250 passed, 0 failed across 28 non-empty suites.

Primary code:

- `crates/heycode-core/src/context.rs`
- `crates/heycode-core/src/plugin.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/src/main.rs`
- `crates/heycode-cli/tests/composition.rs`

Next boundary:

- `B07` is intentionally not folded into this slice; it depends on the authorization service (`A01`) and must validate a credential with its provider rather than add another presence heuristic.
- `B08` waits for the current DeepSeek catalog/provider slice (`PDS01`).
- The next dependency-safe work begins with plugin descriptors (`K01`) so settings, credentials, authorization and UI contributions mount through services instead of becoming CLI special cases.

## 2026-08-24 — K01: built-in plugin descriptors

Tracker: `K01` complete.

Outcome:

- `PluginDescriptor` declares stable id, implementation version, source and broad contribution families.
- `PluginSource` distinguishes compiled built-ins from the temporary `Unclassified` compatibility default.
- Contribution kinds cover services, providers, tools, commands, prompt sections, waterfalls, UI surfaces and managed external processes without pretending to own exact named rows yet.
- All default built-ins and the non-default `session-resume`, `approval-ask` and `sandbox` variants report built-in descriptors with crate versions and non-empty contributions.
- `compose` rejects descriptor-id/legacy-name drift before apply and records each descriptor only after successful apply.
- The real-composition audit pins the complete descriptor/contribution matrix.

Boundary discipline:

- `K02` owns typed service keys.
- `K04` owns exact named contribution attribution and `/plugins verbose`.
- `K05/K06` own scope/profile/config-source metadata.
- External source kinds, manifests and compatibility policy remain with `PL01`; the compatibility default prevents this kernel slice from forcing speculative loader policy.

TDD evidence:

- Red: kernel tests could not resolve descriptor types or audit state.
- Red: the real default world reported every plugin as unclassified before built-ins were migrated.
- Green: mismatch refusal, exact default descriptor matrix and non-default variant coverage.

Primary code:

- `crates/heycode-core/src/descriptor.rs`
- `crates/heycode-core/src/plugin.rs`
- `crates/heycode-core/src/context.rs`
- Built-in plugin constructors across the workspace.
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 252 passed, 0 failed across 28 non-empty suites.

Next boundary:

- `K02` replaces repeated service strings with owner-defined typed constants before new services are introduced.

## 2026-08-24 — K02: typed service keys

Tracker: `K02` complete.

Outcome:

- `ServiceKey` is a const-constructible, validated newtype over stable static names.
- `Context` stores typed keys and requires them for provide/get/has/owner diagnostics.
- `Plugin::inject` is a typed slice, so misspelled dependency strings no longer compile.
- Each service-definition owner exports one `SERVICE_*` constant; the pre-tool seam exports typed `SEAM_PRE_TOOL`.
- All production and test consumers use owner constants. Raw service-string arguments were eliminated from context operations and inject declarations.
- `heycode-cli::BUILTIN_SERVICE_KEYS` lists all 15 current services/seams; its exact-name and uniqueness test makes a duplicate or constitution-table drift visible.

Boundary discipline:

- The key identifies a service contract, not its runtime value type. `ctx.get::<WrongType>(RIGHT_KEY)` still returns `None`; exact contribution/type diagnostics belong to `K04/K08`.
- New services define their constant in the crate owning the service type. Consumers never define aliases or copy the literal.
- String conversion is restricted to config/protocol/diagnostic boundaries through `ServiceKey::as_str()`/`Display`.

TDD evidence:

- Red: core could not resolve `ServiceKey`, and the CLI had no authoritative key registry.
- Green: typed context round-trip, injection behavior, exact/unique built-in registry, real composition, agent flows and binary migration e2e.

Primary code:

- `crates/heycode-core/src/service.rs`
- `crates/heycode-core/src/context.rs`
- `crates/heycode-core/src/plugin.rs`
- Service constants in each owning crate.
- All context/inject consumers across the workspace.
- `crates/heycode-cli/tests/composition.rs`
- `crates/heycode-cli/tests/e2e.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 253 passed, 0 failed across 28 non-empty suites.

Next boundary:

- `K03` makes composition transactional so an Nth-plugin failure unwinds effects registered by plugins 1…N in reverse order.

## 2026-08-24 — K03: transactional composition rollback

Tracker: `K03` complete.

Outcome:

- Every composition failure path routes through one `abort_composition` function.
- Descriptor/name drift, duplicate plugin names, unsatisfied injects and `Plugin::apply` errors all call `Context::shutdown()` before returning the original error.
- If a failing plugin registered effects before returning an error, its partial effects unwind first, followed by prior plugins in strict LIFO order.
- Successfully composed contexts keep their existing explicit, idempotent shutdown behavior.

TDD evidence:

- Red: after plugin three failed, the disposer trace remained empty.
- Green: the trace is exactly `failing-third`, `second`, `first`, and the original plugin error remains visible.
- Real default composition and binary e2e remain green after the kernel change.

Primary code:

- `crates/heycode-core/src/plugin.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 254 passed, 0 failed across 28 non-empty suites.

Next boundary:

- `S01` can now mount the layered settings service on a descriptor-aware, typed-key, transactional kernel.

## 2026-08-24 — S01: layered settings Service Definition

Tracker: `S01` complete.

Outcome:

- Added the `heycode-settings` crate and built-in `settings` plugin/service.
- Plugin-owned namespaces use validated opaque ids, JSON-schema metadata, immutable defaults, authoritative validators, optional composition bases and `live|restart` timing.
- Detached user and trusted-project sections resolve in fixed precedence: schema defaults, base, user, project.
- Objects merge recursively; arrays/primitives replace. Invalid names, non-object sections/defaults, invalid resolved values and duplicates fail before publication.
- Callers receive `Arc<SettingsSnapshot>` with read-only layer/resolved access. Mutating a detached clone cannot mutate the service snapshot.
- Registration requires the live `Context` and installs unregister as an effect, so normal shutdown and K03 rollback own its lifetime.
- The CLI factory/default profile mounts `settings` before consumers and the exact composition/service/descriptor inventories include it.
- Because migration planning consumes the same `BUILTIN_PLUGIN_ORDER`, adding `settings` automatically adds it to the capabilities restored from the historical v0 profile.

Boundary discipline:

- S01 performs no file I/O and exposes no writes, revisions or watchers (`S02/S03`).
- JSON-schema metadata is for future surfaces; the owner validator is authoritative in-process.
- Project documents are labeled trusted input. Trust discovery/enforcement remains `U01/K12`.
- No schema or resolved setting may expose credentials; secret references/redaction remain `S04/S15`.

TDD evidence:

- Red: the new crate exported none of the settings contract.
- Green: four-layer recursive precedence, replacement semantics, validator failures, duplicate rejection, snapshot detachment, effect-owned unregister, plugin descriptor/service publication, real default composition and migration-preview drift tests.

Primary code:

- `crates/heycode-settings/src/`
- `crates/heycode-settings/tests/settings.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 257 passed, 0 failed across 29 non-empty suites.

Next boundary:

- `S02` supplies atomic, comment-preserving user/project files behind this service contract.
- `S04` may proceed independently with credential references/records; values must never enter settings snapshots.

## 2026-08-24 — S02: atomic file settings provider

Tracker: `S02` complete.

Outcome:

- Added `SettingsWriter` and `SettingsService::replace_user` with strict resolve/validate → durable persist → publish ordering.
- Provider errors leave the authoritative snapshot unchanged.
- Added `heycode-settings-file` with a standalone schema-v1 TOML format under `[settings.<namespace>]`.
- Every write re-reads and format-preservingly edits the current document, preserving unrelated comments, unknown root tables and other namespaces.
- Writes serialize in-process and commit through `atomic-write-file`; S03 adds stale-writer revisions/CAS.
- Missing documents are materialized on first write. Existing future schemas, symlinks and non-regular paths fail before publication.
- Unix replacements set `preserve_mode(false)` and mode `0600` on the atomic temporary file, so security metadata and content become visible together.
- Optional trusted-project documents load read-only and keep precedence over user replacements.
- The default CLI world mounts the file-backed `settings-file` runtime variant at an explicit `WorldOptions.settings_user_path`; tests use only temporary paths.

TDD evidence:

- Red: neither the file-provider API nor `replace_user` existed.
- Red: the first serializer emitted an inline namespace table instead of `[settings.agent-runtime]`; conversion to a real `toml_edit::Table` pinned the intended durable shape.
- Green: absent-file creation, comment/unknown preservation, other-namespace survival, project read-only precedence, future-schema refusal, symlink refusal, `0600`, provider-failure no-publish and real default-world persistence.

Primary code:

- `crates/heycode-settings/src/service.rs`
- `crates/heycode-settings/tests/settings.rs`
- `crates/heycode-settings-file/src/`
- `crates/heycode-settings-file/tests/file_provider.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 263 passed, 0 failed across 30 non-empty suites.

Next boundary:

- `S03` adds namespace revisions, expected-revision CAS, external reload publication and serialized watchers without weakening this persist-before-publish boundary.

## 2026-08-24 — S03: settings revision, CAS and watchers

Tracker: `S03` complete.

Outcome:

- Every snapshot carries a raw-user-section revision, initially 0 and checked for overflow.
- `replace_user` accepts an optional expected revision and rejects stale writers before provider persistence or publication with expected/actual conflict evidence.
- Successful user commits persist, advance revision, publish, then notify in one serialized operation lane.
- Context-owned watchers receive previous/next immutable snapshots plus `UserWrite|ProviderReload`; callbacks run in commit order and panics cannot starve later callbacks.
- Synchronous recursive write/publication from a callback fails with `ReentrantWrite` rather than self-deadlocking the operation lane.
- Providers may publish full detached generations. All namespaces validate before any state changes; invalid reloads preserve the complete last-good generation.
- The file plugin starts a `notify` 8.2 parent-directory watcher as an effect, debounces with a fixed ceiling, reloads valid external replacements and stores its latest reload error.
- Watch targets are canonicalized so platform aliases such as macOS `/var` and `/private/var` compare correctly.
- Product worlds enable watching explicitly; composition/e2e tests disable it and the dedicated watcher test owns OS-level proof.

TDD evidence:

- Red: snapshots had no revision, writes accepted no CAS, and no watcher/source types existed.
- Red: the first macOS external-watch test timed out because notify returned canonical `/private/var/...` paths for `/var/...` tempfile inputs.
- Green: stale conflict before writer, revision 0→1→2, callback order/panic containment/disposal, reentrant refusal, valid/invalid provider generation publication, and real external atomic replacement notification.

Primary code:

- `crates/heycode-settings/src/service.rs`
- `crates/heycode-settings/src/model.rs`
- `crates/heycode-settings/tests/settings.rs`
- `crates/heycode-settings-file/src/provider.rs`
- `crates/heycode-settings-file/src/plugin.rs`
- `crates/heycode-settings-file/tests/file_provider.rs`
- `crates/heycode-cli/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 267 passed, 0 failed across 30 non-empty suites.

Next boundary:

- `S04` creates credential references and records; secret values remain outside settings snapshots and diagnostics.

## 2026-08-24 — S04: credentials Service Definition

Tracker: `S04` complete.

Outcome:

- Added `heycode-credentials` with validated reference/kind/provider ids and safe source/validation vocabulary.
- `CredentialSecret` uses `secrecy::SecretString`: explicit exposure, redacted debug, no serialization/display, zeroize on drop.
- `CredentialDescriptor` is serializable safe metadata and structurally has no secret/value field.
- Effect-owned providers resolve deterministically by lower precedence then id; duplicate ids fail.
- Provider `inspect` is separate from secret `resolve`. A configured inspection returning no value is an explicit provider-contract failure.
- The highest-precedence configured provider owns source/writable/validation state. A read-only environment record shadows writable fallback state.
- The credentials plugin mounts an empty registry and a validated `credentials.references` settings namespace containing references only.
- Invalid persisted references fail composition and K03 rolls back the partial plugin load.
- The CLI default profile/service/descriptor/key inventories include credentials, while the legacy env/file ladder remains unmigrated until S05/S07.

TDD evidence:

- Red: no credential types, service, provider contract, plugin or settings namespace existed.
- Green: debug redaction/explicit exposure, descriptor JSON absence of value, provider precedence/shadowing/resolution, duplicate rejection, effect disposal, plugin/settings composition and invalid persisted reference rollback.

Primary code:

- `crates/heycode-credentials/src/`
- `crates/heycode-credentials/tests/credentials.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 271 passed, 0 failed across 31 non-empty suites.

Next boundary:

- `S05` implements the read-only environment provider at precedence 0 and routes current API credential reads through `CredentialsService`.
- `S07` implements/migrates the owner-only file fallback; no provider is called healthy until authorization validation lands.

## 2026-08-24 — S05: environment credential provider

Tracker: `S05` complete.

Outcome:

- Added the `heycode-credentials-env` provider plugin at precedence 0.
- Production reads `std::env::var_os` without process mutation; tests use a deterministic map reader.
- Missing and whitespace-only values are unconfigured. Non-empty values inspect as `Environment`, configured and read-only; non-Unicode values fail only at explicit resolution.
- Added `CredentialsService::write` and provider `write` contract.
- The first configured provider is authoritative for writes. A read-only environment record returns `ShadowedReadOnly`; lower writable providers are not called.
- With no configured record, the first writable provider by precedence is the write target; absence of one fails explicitly.
- The default CLI profile mounts `credentials-env` after the registry and exact plugin/migration inventories include it.

TDD evidence:

- Red: no environment provider, provider write method, service write selector or shadow error existed.
- Green: non-empty/blank inspection, explicit resolution, precedence-0 descriptor, lower-writer non-invocation, provider/plugin composition and migration-preview drift.

Primary code:

- `crates/heycode-credentials/src/provider.rs`
- `crates/heycode-credentials/src/service.rs`
- `crates/heycode-credentials-env/src/lib.rs`
- `crates/heycode-credentials-env/tests/environment.rs`
- `crates/heycode-cli/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 273 passed, 0 failed across 32 non-empty suites.

Next boundary:

- `S06` adds the writable OS keychain provider below environment.
- `S07` adds/migrates the owner-only file fallback and removes the legacy CLI key ladder.

## 2026-08-24 — S06: OS keychain credential provider

Tracker: `S06` complete.

Outcome:

- Added `heycode-credentials-keychain` at precedence 10 using keyring 4.1.6.
- Stable entry naming is service `heycode`, account = credential reference.
- Added credential-provider/service delete contracts with the same authoritative-shadow semantics as write.
- Supported stores inspect absent records as unconfigured/writable; set→inspect→resolve→delete round trips through the service.
- No default/unsupported store becomes unconfigured/non-writable, allowing S07 fallback. Locked/broken operational errors remain loud.
- Keyring's existence check internally retrieves then zeroizes its temporary String because the native abstraction exposes no metadata-only lookup.
- A public backend trait keeps ordinary tests deterministic and prevents OS prompts/mutations.
- The default profile mounts `credentials-keychain` below environment; plugin/migration inventories are exact.

Live evidence:

- On macOS, `HEYCODE_E2E=1` initialized the real native store and inspected/resolved the deliberately nonexistent account `HEYCODE_E2E_NONEXISTENT_KEYCHAIN_PROBE_7F5A` as absent. The probe performed no write/delete.

TDD evidence:

- Red: no keychain backend/provider/plugin or credential delete route existed.
- Green: unavailable state, precedence 10, writable absent descriptor, service write/read/delete round trip, source metadata, default composition and gated native read-only probe.

Primary code:

- `crates/heycode-credentials/src/provider.rs`
- `crates/heycode-credentials/src/service.rs`
- `crates/heycode-credentials-keychain/src/lib.rs`
- `crates/heycode-credentials-keychain/tests/keychain.rs`
- `crates/heycode-cli/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 276 passed, 0 failed across 33 non-empty suites on macOS.
- Gated native read-only Keychain probe — pass; no item was created or changed.

Next boundary:

- `S07` provides a secure file fallback and migrates legacy `KEY=value` credentials without overriding environment or a usable keychain.

## 2026-08-24 — S07: owner-only credential fallback and migration

Tracker: `S07` complete.

Outcome:

- Added `heycode-credentials-file` at precedence 20 with schema-v1 `credentials.toml`.
- Unix root/file enforcement is `0700`/`0600`; symlinks, wrong types and future schemas fail at plugin load.
- Read/write/delete re-read each operation, so externally rotated file values affect the next resolution.
- Secret-bearing raw/serialized Strings are zeroized. A custom secret-map drop guard wipes every value.
- Legacy `REFERENCE=value` migration merges missing/equal records, rejects conflicts, commits new, creates/verifies a byte-exact backup, then removes legacy active state. Re-entry is idempotent.
- Default composition mounts `credentials-file` below Keychain and environment.
- Real LLM adapters now construct inside the `llm` plugin after credential provider application and resolve the configured reference from `CredentialsService`.
- Bootstrap preflight/setup use the same environment→Keychain→file service stack; setup writes default to Keychain when available and existing file records stay authoritative.
- Explicit `credentials_root` keeps every composition/e2e test isolated from the real home.

Live migration evidence (no values printed):

- Offline real-home `--fake` smoke passed.
- `/Users/naresh/.heycode` is `0700`.
- `credentials.toml` and `credentials.legacy.bak` are `0600`; legacy active `credentials` is absent.
- Safe reference inventory contains `OPENROUTER_API_KEY`.
- `config.toml` is schema 1 and `config.toml.unversioned.bak` exists.

TDD evidence:

- Red: no file provider/config/plugin or migration contract existed.
- Green: 0700/0600 repair, write/read/delete, exact backup, conflict preservation, idempotence, symlink refusal, bootstrap subsequent-read, and real LLM construction after migration.

Primary code:

- `crates/heycode-credentials-file/src/`
- `crates/heycode-credentials-file/tests/file_credentials.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/src/main.rs`
- `crates/heycode-cli/tests/composition.rs`
- `crates/heycode-cli/tests/e2e.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 282 passed, 0 failed across 34 non-empty suites on macOS.
- Real-home offline migration smoke — pass; exact path/mode/reference checks recorded above.

Next boundary:

- `A01/S08` define authorization flows; `S09` validates API keys live before committing/claiming health.

## 2026-08-24 — U02: integrated TUI onboarding state machine

Tracker: `U02` complete.

Outcome:

- Added `heycode-onboarding` with a plugin-owned generic state machine, immutable view snapshots, keyboard-neutral actions and semantic outcomes.
- Welcome offers Connect/Exit; Connect advances to Subscription, API or router, Cloud and Local runtime classes matching the UI specification.
- The TUI stores/renders only the snapshot and forwards arrows/Tab/Enter/Esc to the service. Provider-specific connector logic is absent from rendering.
- Active onboarding renders a centered Claude-style card and blocks the composer/transcript input surface.
- Interactive missing-credential startup now activates onboarding instead of invoking the old line prompt.
- Real composition permits a disconnected provider only while onboarding is required, so agent/TUI services can load; any attempted stream fails with actionable “finish onboarding” text.
- Headless/ACP missing credentials continue to fail before interaction.
- `WorldOptions.onboarding_required` is explicit; ordinary tests stay inactive.
- Double Ctrl+C and Esc terminal restoration remain correct.

Live evidence:

- Isolated real-binary PTY showed Welcome, Enter advanced to the four runtime classes, and Esc exited/restored the alternate screen. No real home or network was used.

TDD evidence:

- Red: no onboarding service/types/plugin or TUI state/render methods existed.
- Green: state transitions/outcomes, plugin publication, Welcome/runtime class frames, composer blocking, double Ctrl+C, disconnected real-world composition and PTY smoke.

Primary code:

- `crates/heycode-onboarding/src/lib.rs`
- `crates/heycode-onboarding/tests/state_machine.rs`
- `crates/heycode-tui/src/app.rs`
- `crates/heycode-tui/src/render.rs`
- `crates/heycode-tui/tests/onboarding.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/src/main.rs`
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 288 passed, 0 failed across 36 non-empty suites on macOS.
- Isolated real-binary PTY Welcome→runtime-class→Esc smoke — pass.

Next boundary:

- `S08` registers connector-owned authorization flows into this wizard.
- `U05` contributes concrete runtime/provider rows and `U06/U07/U09` supply auth/model/permission pages.

## 2026-08-24 — S08: authorization flow registry

Tracker: `S08` complete.

Outcome:

- Added `heycode-authorization` with validated flow ids, safe descriptors, method taxonomy, requests/grants/receipts and typed errors.
- Flow registrations are unique context effects and descriptor catalogs are deterministic.
- One caller-owned `CancellationToken` covers the invocation. Pre-cancel skips the flow; post-grant cancellation drops/zeroizes the grant before any write.
- Flows return a secret grant only. The registry owns credential persistence.
- Success requires `CredentialsService::write`, safe descriptor readback and proof that the written provider is configured and still authoritative.
- Receipts contain flow id, committed provider id and safe credential descriptor; no secret/value field or debug leak.
- The empty registry mounts after credentials and before onboarding in the default profile, ready for connector flows.

TDD evidence:

- Red: no authorization types/service/plugin existed.
- Green: unique/duplicate lifecycle, unknown-safe catalog, pre/post cancellation with zero writes, committed write/readback proof, secret-free receipt and default composition inventory.

Primary code:

- `crates/heycode-authorization/src/`
- `crates/heycode-authorization/tests/authorization.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 291 passed, 0 failed across 37 non-empty suites on macOS.

Next boundary:

- `S09` contributes the masked API-key flow with live validation classifications.
- `U06` renders authorization descriptors/pages and consumes semantic receipts.

## 2026-08-24 — S09: masked API-key authorization and live validation

Tracker: `S09` complete.

Outcome:

- Added `heycode-authorization-api-key` with masked secret-prompt and validator interfaces.
- API-key flows own an exact credential query, reject mismatched references, request masked input, validate live and return a validated grant only.
- Stable failures are unauthorized, host, model, network and cancelled; authorization errors carry the code without provider bodies.
- OpenRouter validator uses official `/api/v1/key` plus optional `/models`; DeepSeek uses authenticated `/models`.
- Status classification: 401/403 unauthorized; 404/bad URL/bad JSON host; authenticated missing model model; transport/timeout/429/5xx network.
- Successful checked-at evidence is recorded against the committed provider record and appears in authoritative receipt descriptors.
- Failed validation never calls a credential writer.
- Default composition registers discoverable OpenRouter/DeepSeek flow descriptors through `authorization-api-key`; the prompt is deliberately deferred/fail-safe until U06 mounts masked TUI input.

Live evidence:

- Gated official OpenRouter request with a deliberately invalid probe key returned/classified unauthorized in ~70 ms; no value/body was logged or committed.

TDD evidence:

- Red: masked prompt/validator/flow types and stable failure codes did not exist.
- Green: masking assertion, validation evidence commit, four-class no-write matrix, HTTP status/model fixtures, official invalid-key probe and default flow catalog.

Primary code:

- `crates/heycode-authorization-api-key/src/lib.rs`
- `crates/heycode-authorization-api-key/tests/api_key.rs`
- `crates/heycode-authorization/src/{flow,model,error,service}.rs`
- `crates/heycode-credentials/src/{model,service}.rs`
- `crates/heycode-cli/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 295 passed, 0 failed across 38 non-empty suites on macOS.
- Gated official OpenRouter deliberately-invalid-key classification — pass (`unauthorized`, no commit).

Next boundary:

- `U06` replaces the deferred prompt with the masked TUI page and drives selected flow receipts.
- `B07/S12` validate/cache existing configured credentials before a normal composer/turn.

## 2026-08-24 — U06: authorization method and masked input pages

Tracker: `U06` complete.

Outcome:

- Added `InteractiveSecretPrompt`: event-driven request/answer/cancel broker with safe notifications and oneshot secret delivery.
- Added `secret-prompt` service/plugin and mounted it before API-key flow registration; default flows use the live broker rather than a deferred stdin fallback.
- TUI renders authorization methods dynamically from `AuthorizationService` descriptors filtered by Subscription/API/Cloud/Local class.
- Selecting a flow invokes S08 asynchronously with its descriptor-owned safe credential query.
- Masked card shows prompt/reference and bullets only. Raw input is private, 8192-character capped and zeroized on cancel/drop; submit moves directly into `CredentialSecret`.
- Broker/UI events and debug contain no secret text. Resolved events contain only `answered: bool`.
- Safe validation failures return to the method page as notices. Success shows Connected/restart, accurately deferring hot agent recomposition to U10.

Live evidence:

- Isolated PTY: Welcome → API/router → OpenRouter API key → masked card. Every character rendered as one bullet; the deliberately invalid key produced safe `unauthorized` text with no key/body visible; Esc restored the terminal.

TDD evidence:

- Red: broker/notification service and TUI secret state/render APIs did not exist.
- Green: safe broker metadata/answer, bullet-only frame, submit clearing, runtime-class method filters, dynamic method frame, authorization invocation, error notice and real PTY.

Primary code:

- `crates/heycode-authorization-api-key/src/lib.rs`
- `crates/heycode-authorization-api-key/tests/api_key.rs`
- `crates/heycode-onboarding/src/lib.rs`
- `crates/heycode-onboarding/tests/state_machine.rs`
- `crates/heycode-tui/src/{app,render}.rs`
- `crates/heycode-tui/tests/{onboarding,secret_prompt}.rs`
- `crates/heycode-cli/src/{lib,main}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 299 passed, 0 failed across 39 non-empty suites on macOS.
- Isolated real PTY method selection + bullet-only invalid-key + safe unauthorized notice — pass.

Next boundary:

- `U07` persists provider selection; `U10` hot-recomposes the connected provider and reaches the ready composer.

## 2026-08-24 — S12 + B07: validation cache and stale-key preflight

Tracker: `S12` and `B07` complete.

Outcome:

- Credential validation records now contain safe state, expiry and a private SHA-256 secret fingerprint.
- Descriptors project Valid until expiry, then Stale with the original check time.
- The next secret resolution compares fingerprints; external rotation clears cached validation immediately. Service write/delete and provider disposal also invalidate.
- S08 records validated grants with a 15-minute TTL; disconnected/unknown grants remain Unknown.
- Binary startup validates an existing configured OpenRouter/DeepSeek credential before normal composition/turn.
- Successful preflight passes its safe timestamp into the real LLM plugin, which seeds the long-lived composed cache against the resolved provider/secret.
- Interactive invalid state enters onboarding repair; headless/ACP fails before model dispatch.

Live evidence:

- Real migrated OpenRouter credential classified unauthorized; headless exited with safe text before any model turn. No key/body printed.

TDD evidence:

- Red: validation records had no TTL/fingerprint and record API accepted no secret/TTL; Stale did not exist.
- Green: Valid→Stale projection, external secret rotation→Unknown on next resolve, mutation/disposal invalidation, authorization cache seeding and real stale-key preflight.

Primary code:

- `crates/heycode-credentials/src/{model,service}.rs`
- `crates/heycode-credentials/tests/credentials.rs`
- `crates/heycode-authorization/src/service.rs`
- `crates/heycode-cli/src/{lib,main}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 300 passed, 0 failed across 39 non-empty suites on macOS.
- Real migrated stale OpenRouter headless preflight — safe unauthorized exit before model dispatch.

Next boundary:

- `U10` orchestrates successful authorization into hot recomposition; `S11` establishes the doctor plane that later provider/credential checks use for validation/cache state.

## 2026-08-24 — CAT01: provider/model descriptors and unknown semantics

Tracker: `CAT01` complete.

Outcome:

- Added provider/model descriptor vocabulary to `heycode-llm` without changing request/chunk types.
- `CapabilitySupport` is Supported/Unsupported/Unknown; Unknown converts to no boolean and is never optimistic support.
- `ModelCapabilities` covers tools, reasoning, image input, structured output, native web, native compaction and prompt cache.
- Unknown model descriptors preserve id/display identity and leave limits/capabilities unknown.
- Provider descriptors declare id/display/protocol families.
- Provider trait defaults preserve identity with protocol Unknown and conservative model descriptors.
- DeepSeek/OpenRouter declare OpenAI Chat Completions protocol only; model capabilities remain unknown until CAT02 evidence.

TDD evidence:

- Red: descriptor/capability/protocol types and Provider methods did not exist.
- Green: tri-state conversions, unknown descriptor identity, minimal-provider defaults and shipped-adapter protocol/no-capability-guess contracts.

Primary code:

- `crates/heycode-llm/src/catalog.rs`
- `crates/heycode-llm/src/provider.rs`
- `crates/heycode-llm/src/{deepseek,openrouter}.rs`
- `crates/heycode-llm/tests/descriptors.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 304 passed, 0 failed across 40 non-empty suites on macOS.

Next boundary:

- `CAT02` supplies live catalogs/TTL/single-flight refresh; `CAT03` adds lifecycle/retirement and `CAT04` persists selection separately.

## 2026-08-24 — CAT02: catalog registry, TTL cache and single-flight refresh

Tracker: `CAT02` complete.

Outcome:

- Added the provider-owned async `ModelCatalog` source boundary and the plugin-owned `CatalogRegistry` service under typed key `models`.
- Catalog registration is a context effect. Disposal removes the exact registration, cancels its source operation and settles active waiters; the built-in profile now mounts the base `models` plugin before inference.
- `PreferCache` observes a five-minute built-in TTL; `Force` bypasses fresh cache. Concurrent live refreshes for one provider share exactly one source call.
- Caller cancellation stops only that waiter. The registry owns the shared source cancellation token and a supervised teardown handle.
- Successful generations are immutable, id-sorted, timestamped and provider-locally revisioned. Blank/duplicate ids, failures and panics never replace last-good.
- Ordinary refresh failures return visible `StaleFallback` plus the safe classified error when last-good exists. Forced failures and cancellation stay errors.

TDD evidence:

- Red: the new integration suite could not import the catalog source, registry, modes, freshness, errors, plugin or `SERVICE_MODELS` contracts.
- Green: exact one-call concurrency, TTL hit/forced bypass, stale warning/forced error, caller-only cancellation, invalid-generation last-good retention, registration uniqueness/disposal, in-flight shutdown and real plugin publication.

Primary code:

- `crates/heycode-llm/src/model_catalog.rs`
- `crates/heycode-llm/tests/catalog_registry.rs`
- `crates/heycode-llm/src/lib.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 312 passed, 0 failed across 41 non-empty suites on macOS.
- Fresh temporary-home `cargo run --quiet -- --fake run ...` — real binary composed the new default `models` plugin and completed `[done:stop]`.
- Official OpenRouter `GET /api/v1/models` reconnaissance — 419 rows; `stealth/ox-alpha` present with 1,048,576 context, 131,072 output cap and explicit tools/reasoning/response-format/image evidence. Provider normalization remains `POR02`, after its declared dependencies.

Next boundary:

- `CAT03` adds explicit model lifecycle/deprecation/retirement semantics; `CAT04` persists last-good catalog generations separately from user selection. Provider-specific live sources remain under `POR02`, `PDS01` and their sibling provider tasks.

## 2026-08-24 — CAT03: lifecycle, retirement and configured selection

Tracker: `CAT03` complete.

Outcome:

- Added explicit `Unknown | Stable | Preview | Deprecated | Retired` lifecycle evidence plus optional retirement instant and ordered provider replacement ids.
- Lifecycle resolution takes an explicit Unix-millisecond instant. A cached Deprecated descriptor becomes effectively Retired at its own deadline without mutating the committed snapshot.
- Added provider-recognized model aliases. Selection resolves id/alias to one canonical descriptor; alias/id collisions invalidate the candidate generation.
- `CatalogRegistry::resolve_model` fails absent configured ids and effectively retired ids/aliases before transport, returning at most five deterministic canonical alternatives: valid provider recommendations first, then stable/preview/unknown/deprecated rows.
- Deprecated models before their deadline remain selectable with a visible retirement/replacement warning. Unknown lifecycle remains unknown and is not converted to unsupported.

TDD evidence:

- Red: lifecycle/status/warning/error contracts, the descriptor field and catalog-backed resolver did not exist.
- Red: a configured provider alias could not be represented or resolved.
- Green: unknown preservation, deadline-based refusal, canonical alias resolution, ordered alternatives, future-deprecation warning and missing-id refusal.

Primary code:

- `crates/heycode-llm/src/catalog.rs`
- `crates/heycode-llm/src/model_selection.rs`
- `crates/heycode-llm/src/model_catalog.rs`
- `crates/heycode-llm/tests/model_lifecycle.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 316 passed, 0 failed across 42 non-empty suites on macOS.
- Official OpenRouter model schema reconfirmed `expiration_date` as the endpoint deprecation date; provider-specific ISO-8601 normalization remains `POR02`.

Next boundary:

- `CAT04` persists catalog generations separately from configured provider/model ids; `P01` consumes this resolver in explicit pre-transport call resolution once provider catalog sources are mounted.

## 2026-08-24 — CAT04: durable catalog cache separate from selection

Tracker: `CAT04` complete.

Outcome:

- Added one effect-owned `CatalogPersistence` provider slot to the `models` registry. Registration validates/restores complete generations and merges newer in-memory state; disposal removes only the provider capability.
- Successful refresh now serializes persistence commits and publishes revision/cache only after durable success. Failed persistence leaves last-good untouched; restored revision continues monotonically.
- Added `heycode-catalog-file`, plugin `catalog-cache-file`, and mounted it in the built-in profile after `models` at `$HEYCODE_HOME/cache/models.json`.
- File schema v1 carries provider/model metadata, canonical aliases, lifecycle/capabilities, provider-local revision and commit timestamp. It has no selection/provider/model root fields; `[llm]` remains the independent user reference owner.
- Reads/writes are capped at 32 MiB, symlinks/non-files are refused, parent/file modes are `0700/0600` on Unix, replacement is atomic, and cancellation before commit keeps byte-exact last-good.
- Version is probed before strict v1 decoding, so future documents with new fields fail as newer schema without rewrite.

TDD evidence:

- Red: persistence trait/error/registration/health contracts did not exist.
- Red: the file crate had no library/plugin implementation.
- Red: strict v1 decoding misclassified a future document with an added field as corruption.
- Green: durable restore without source call, monotonic revision continuation, failed-save no-publication, unique effect lifecycle, complete JSON round trip, owner modes, cancellation byte preservation, future-schema refusal, symlink refusal and default composition inventory.

Primary code:

- `crates/heycode-llm/src/model_catalog.rs`
- `crates/heycode-llm/tests/catalog_persistence.rs`
- `crates/heycode-catalog-file/src/{lib,config,error,wire,store,plugin}.rs`
- `crates/heycode-catalog-file/tests/file_catalog.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-cli/tests/{composition,e2e}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 324 passed, 0 failed across 44 non-empty suites on macOS.
- Fresh temporary-home offline binary — default `models` + `catalog-cache-file` composition completed `[done:stop]`; cache directory existed owner-only and no empty cache document was fabricated before a generation existed.

Next boundary:

- `CAT05` provides capability/lifecycle-aware query filters for the model picker. Provider-specific source plugins then populate this durable cache under their declared dependency paths.

## 2026-08-24 — CAT05: unknown-safe capability and lifecycle filters

Tracker: `CAT05` complete.

Outcome:

- Added explicit `CapabilityFilter::{Any,Supported,Unsupported,Unknown}` predicates for tool calling, image input and reasoning evidence.
- Added `ModelLifecycleFilter::{Any,Selectable,Stable,Preview,Deprecated,Retired,Unknown}` with lifecycle evaluated at caller-supplied Unix milliseconds.
- `ModelFilter::all()` includes every row for diagnostics; `ModelFilter::selectable()` excludes only effective retirement. Stable remains a separate, stricter UI choice.
- `CatalogSnapshot::filter_models` composes every predicate and preserves deterministic catalog order without cloning descriptors.

TDD evidence:

- Red: filter types and the snapshot query API did not exist.
- Green: Supported/Unsupported/Unknown separation, combined tool+image constraints, stable-vs-selectable behavior and exact deadline transition into the Retired result set.

Primary code:

- `crates/heycode-llm/src/model_filter.rs`
- `crates/heycode-llm/tests/model_filter.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 328 passed, 0 failed across 45 non-empty suites on macOS.

Next boundary:

- `U07` consumes these filters in the live picker after the UI contribution registry; provider-specific catalog tasks populate authoritative fields.

## 2026-08-24 — P01: explicit inference resolution and one-shot call

Tracker: `P01` complete.

Outcome:

- Added the native-only `InferenceAdapter` contract; delegated Codex/Claude/OpenCode runtimes remain a separate future `AgentRuntime` contract.
- Added durable-projection `RequestDraft`, adapter-owned `ResolveSpec`, private non-Clone `ResolvedCall`, secret-free authentication bindings, HTTP/managed targets, call purpose, input/native feature vocabulary and opaque reasoning/credential ids.
- `resolve_request` validates provider/model/alias/protocol/lifecycle, target safety, non-empty modalities, tool schema/name uniqueness, finite sampling, output bounds and adapter defaults before transport.
- Tools, image, reasoning, structured output, native web/compaction and prompt cache require explicit Supported evidence. Unsupported and Unknown produce distinct `Unsupported` and `Unproven` errors.
- Exact reasoning effort ids are validated against adapter-owned choices and never clamped. Adapter reasoning/output defaults are materialized and recorded in `ResolvedDefaults`.
- A resolved call is consumed by `stream`, giving one-shot dispatch by Rust ownership. Existing `Provider` adapters remain on the legacy path until P02/P04 can translate every represented field without silent loss.

TDD evidence:

- Red: all inference draft/spec/adapter/call/error types and the resolver were absent.
- Green: Unsupported-vs-Unproven tool failures with zero dispatch, six requested capability refusals, exact effort rejection, valid default materialization/dispatch, provider/protocol/retirement/output-limit failures, malformed schema/target/default and empty-input refusal.

Primary code:

- `crates/heycode-llm/src/inference.rs`
- `crates/heycode-llm/tests/inference_resolution.rs`
- `crates/heycode-llm/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 334 passed, 0 failed across 46 non-empty suites on macOS.

Next boundary:

- `P02` separates transport mechanics from provider semantics. `P03`/`P04` then dispatch `ResolvedCall` through Responses and Chat Completions without dropping fields; `P09` replaces adapter-owned legacy auth with operation-time credential handles.

## 2026-08-25 — P02: plugin-owned HTTP/SSE transport split

Tracker: `P02` complete.

Outcome:

- Added lower-layer crate/service `heycode-http` and built-in plugin `http-reqwest`; the default composition now publishes typed service `http` before credentials/providers.
- `HttpSseRequest` carries opaque body/header bytes without Debug, validates HTTP(S) targets and header syntax, and never interprets provider JSON.
- `ReqwestHttpTransport` owns request execution, pre/mid-stream cancellation, success content-type validation, bounded non-success body tails, network errors and raw event streaming.
- `SseDecoder` owns arbitrary byte fragmentation (including split multibyte UTF-8), BOM, CR/LF/CRLF, comments, persistent ids, retry hints, multi-`data:` joining, EOF flush and a one-MiB event cap.
- OpenAI Chat parsing now consumes raw `SseEvent`; only `heycode-llm` interprets `[DONE]`, JSON deltas, DeepSeek reasoning, tool fragments, usage and finish ordering.
- Credential-composed DeepSeek/OpenRouter providers receive the shared `HttpService`. Compatibility constructors still create a local reqwest implementation for standalone embedders/tests.
- Existing mock-wire headers/body/status/stream behavior remains green; a shared-transport test proves both branded adapters use the same transport while protocol meaning stays above it.

TDD evidence:

- Red: the `heycode-http` crate/API did not exist.
- Red: an invalid UTF-8 framing failure did not close the decoder.
- Red: successful `application/json` was silently accepted by an SSE operation.
- Green: every byte split and one-byte reads, multi-data/UTF-8/id/retry/comments, EOF, invalid/capped events, raw fragmented localhost transport, pre/mid cancellation, bounded 429, non-SSE refusal, legacy protocol split invariance and two-adapter injection.

Primary code:

- `crates/heycode-http/src/{lib,sse,transport,plugin}.rs`
- `crates/heycode-http/tests/{sse_decoder,http_sse}.rs`
- `crates/heycode-llm/src/{sse,wire,deepseek,openrouter}.rs`
- `crates/heycode-llm/tests/shared_transport.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/composition.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 345 passed, 0 failed across 49 non-empty suites on macOS.
- Fresh temporary-home offline binary — composed `http-reqwest` and completed `[done:stop]`.

Next boundary:

- `P03` implements OpenAI Responses semantics over this raw transport; `P04` migrates Chat Completions onto `ResolvedCall` and retires the compatibility-only byte facade.

## 2026-08-25 — P03: OpenAI Responses protocol adapter

Tracker: `P03` complete.

Official source baseline:

- [Create a model response](https://developers.openai.com/api/reference/typescript/resources/beta/subresources/responses/methods/create) — request fields, typed streaming lifecycle, output items, function tools, reasoning, structured output and terminal usage.
- [Current model guidance](https://developers.openai.com/api/docs/guides/latest-model) — stateless/ZDR replay of user inputs and every output item, encrypted reasoning continuity and `phase` preservation.

Outcome:

- Added reusable `OpenAiResponsesAdapter`/redacted config over the composed raw HTTP service; no OpenAI provider/profile is mounted yet.
- Evolved native inference streaming to `InferenceEvent` with response/item start/done phases, text/reasoning deltas, typed function-call deltas, lossless provider state, terminal usage and finish.
- Replaced parallel message/state fields with ordered `InferenceInput::{Message,ProviderState}` so stateless replay preserves chronology.
- `ProviderStateItem` carries provider, canonical model, Responses protocol, schema version, kind and lossless JSON; route/protocol/schema mismatch fails resolution.
- Request serialization covers instructions, chronological state/messages/function call outputs, client functions, web search, reasoning effort, structured JSON schema, sampling/output cap, store=false, encrypted reasoning include and model-hidden purpose metadata. Image/compaction/cache requests fail until their representations land.
- Stateful parsing validates contiguous sequence after the first event, SSE event/type agreement, response/item identity and settlement, exact function argument reassembly and terminal response status.
- Completed output items—including encrypted reasoning and `phase`—emit lossless state. `response.incomplete/max_output_tokens` maps to Length; failed/cancelled/out-of-order/EOF paths never synthesize success.

TDD evidence:

- Red: Responses adapter/config/event/state/item vocabulary and ordered provider input did not exist; raw HTTP request inspection was unavailable to replaceable transports.
- Green: request/state/tool/reasoning/schema/web serialization, secret-free debug/body, full phase/state/function/usage stream, sequence/failure refusal, max-output terminal mapping, EOF refusal, `phase` preservation and explicit unsupported feature gates.

Primary code:

- `crates/heycode-llm/src/{inference,responses}.rs`
- `crates/heycode-llm/tests/{inference_resolution,responses_protocol}.rs`
- `crates/heycode-http/src/transport.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 352 passed, 0 failed across 50 non-empty suites on macOS.

Next boundary:

- `P04` implements the same normalized adapter contract for Chat Completions. `POA01` later contributes OpenAI auth/catalog/profile; `C01` makes Responses state durable before any product loop uses it.

## 2026-08-25 — P04: OpenAI Chat Completions protocol adapter

Tracker: `P04` complete.

Official source baseline:

- [Chat Completions API reference](https://developers.openai.com/api/reference/cli/resources/chat/subresources/completions) — message roles, streamed chunks, function tools/tool results and `stream_options.include_usage`.

Outcome:

- Added reusable `OpenAiChatCompletionsAdapter` and redacted config over `ResolvedCall`/`InferenceEvent`/shared HTTP.
- Added explicit `ChatReasoningWire::{None,ObjectEffort,ScalarEffort}`; exact reasoning choices/defaults cannot be advertised without a request dialect.
- Request serialization preserves chronological neutral messages and lossless Chat assistant state, system prompt, function tools/results, reasoning controls, sampling/output cap, usage streaming and extra attribution/gateway headers.
- Stateful stream parsing enforces one choice at index zero, stable response id, finish/usage uniqueness, no deltas after finish and EOF settlement.
- Text, `reasoning_content` and interleaved parallel tool fragments normalize. Tool id/name are fixed by the first fragment; completed arguments/state are fully reassembled.
- Every successful choice emits a complete schema-v1 `ChatAssistantMessage` provider-state item containing visible text/null, reasoning and ordered function calls for lossless continuation.
- Existing DeepSeek/OpenRouter types now implement both legacy `Provider` and new `InferenceAdapter` over the same shared transport. The legacy client uses the normalized parser and projects only v1-understood events; state activation still waits for C01.
- Image, structured-output and native-feature dialects fail explicitly until provider-specific configurations implement them.

TDD evidence:

- Red: Chat adapter/config/reasoning dialect and Chat assistant provider state did not exist.
- Green: ordered replay request, tools/reasoning/controls/headers, text+reasoning+two interleaved tools, complete replay state, usage/finish, malformed choice/identity/late-delta/EOF refusal, unsupported representation gates and branded-provider inference conformance.

Primary code:

- `crates/heycode-llm/src/{chat,inference,wire,deepseek,openrouter}.rs`
- `crates/heycode-llm/tests/{chat_protocol,descriptors,mock_server,shared_transport}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 358 passed, 0 failed across 51 non-empty suites on macOS.
- Fresh temporary-home offline binary — completed `[done:stop]` through the compatibility projection after parser migration.

Next boundary:

- `P05` adds Anthropic Messages blocks/thinking/server tools/pause semantics. `POR01` and `PDS01` can now build provider profiles on P04; C01 remains required before lossless state is used by the product loop.

## 2026-08-25 — C01: session envelope v2 and v1 read migration

Tracker: `C01` complete.

Outcome:

- Exported readable range v1..=v2; every new session append now writes envelope `v:2` with the existing `{v,seq,time_ms,kind,data}` shape.
- Reader selects a closed known-kind set by each line's source version and deserializes both into the current exhaustive `SessionEventKind` payload model.
- Existing v1 lines retain `event.v == 1` in memory as source provenance and remain byte-exact on disk. Resuming then appending produces a valid v1 prefix followed by v2 lines.
- Version order is monotonic: v2→v1 regression, v0/v3, unknown per-version kinds, corrupt JSON and sequence gaps fail loud.
- Added a checked-in golden v1 fixture covering all 12 legacy kinds and provider projection; replay remains identical. Added exhaustive current-kind v2 append/reopen round trip.
- C01 deliberately adds no v2-only event kinds. C02 request events and C03 provider state extend only the v2 kind gate next.

TDD evidence:

- Red: current-version export and version-regression error did not exist; v2 was rejected.
- Green: golden v1 projection + byte preservation + v2 continuation/reopen, every current kind v2 round trip, version regression and outside-range refusal.

Primary code:

- `crates/heycode-session/src/{event,session,lib}.rs`
- `crates/heycode-session/tests/version_migration.rs`
- `crates/heycode-session/tests/fixtures/session-v1.jsonl`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 361 passed, 0 failed across 52 non-empty suites on macOS.

Next boundary:

- `C02` adds complete `request/header` and `request/context` v2 events; `C03` adds lossless provider-state events. Both remain impossible in a v1 envelope by construction.

## 2026-08-25 — C02: durable request header and context

Tracker: `C02` complete.

Outcome:

- Added opaque core `RequestId` and moved shared `ProviderProtocol` to `heycode-core`, the lowest common dependency for LLM/session. `ToolSpec` is now serde-capable for exact schema persistence.
- Added v2-only `request/header` with turn/step/request correlation and boxed full `RequestHeaderSnapshot`.
- Header stores provider/canonical model/protocol, HTTP or managed target, secret-free auth binding/reference, exact rendered system prompt + verified SHA-256, complete ordered tool schemas and explicit modalities/reasoning/schema/native/sampling/output/purpose options.
- Added v2-only `request/context` with correlated context window, model output maximum and paired catalog revision/commit timestamp.
- Snapshot constructors and append/read validation reject blank/duplicate/ambiguous fields, embedded HTTP authority credentials, prompt tampering, non-object schemas, non-finite temperature, zero limits and unpaired catalog evidence.
- V1 kind gate rejects both request kinds. V2 exhaustive fixture includes them. The neutral legacy message projection explicitly ignores both until C04.

TDD evidence:

- Red: shared protocol/request id, snapshots, event variants and semantic event errors did not exist.
- Green: complete append/reopen field round trip, v2-only gating, prompt/hash tamper refusal, invalid tool/options/context constructors and full-workspace exhaustive consumers.

Primary code:

- `crates/heycode-core/src/{id,vocab,lib}.rs`
- `crates/heycode-session/src/{request,event,session,projection,lib}.rs`
- `crates/heycode-session/tests/{request_snapshot,version_migration}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 365 passed, 0 failed across 53 non-empty suites on macOS.

Next boundary:

- `C03` persists provider-owned continuation items under `RequestId`; `C04` then reconstructs ordered protocol input from messages + those items + request snapshots.

## 2026-08-25 — C03: lossless provider-state event

Tracker: `C03` complete.

Outcome:

- Moved `ProviderStateKind`, `ProviderStateItem` and typed validation errors from LLM to `heycode-core`, the lowest common emitter/persistence dependency. LLM reexports the same types.
- Added v2-only `assistant/provider-item` with turn/step, `RequestId`, ordered output index and boxed lossless state.
- Core state records provider, canonical model, protocol, kind, schema version and exact object JSON. Responses output-item state requires Responses protocol + non-empty item `type`; Chat assistant state requires Chat protocol + role `assistant`.
- Append/read validation rejects blank identities, non-object data, schema drift and protocol-kind substitution.
- Golden tests round-trip encrypted Responses reasoning/phase and DeepSeek-style Chat reasoning/tool calls exactly and in order. V1 envelope rejects the kind.
- Legacy neutral message projection explicitly ignores provider items; C04 adds the protocol-aware ordered projection next.

Regression caught during TDD:

- The first insertion accidentally added `assistant/provider-item` to `KNOWN_KINDS_V1` because the arrays share nearby text. The v1-only rejection test failed. The kind now exists only in v2, and GOTCHAS #52 records the hazard.

Primary code:

- `crates/heycode-core/src/{vocab,lib}.rs`
- `crates/heycode-llm/src/{inference,lib}.rs`
- `crates/heycode-session/src/{event,projection}.rs`
- `crates/heycode-session/tests/{provider_state,version_migration}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 369 passed, 0 failed across 54 non-empty suites on macOS.

Next boundary:

- `C04` constructs ordered neutral/provider inputs per request id and protocol; C05 independently compares reconstructed snapshots with the live resolved call before transport.

## 2026-08-25 — C04: protocol-aware request projection

Tracker: `C04` complete.

Outcome:

- Added `ProjectedRequest` keyed by turn/step/`RequestId`, carrying validated header, context and ordered `ProjectedInput::{Message,ProviderState}`.
- `project_requests` requires one unique header then one context; provider items require a preceding producing header, exact provider/model/protocol + turn/step match and contiguous output indices from zero.
- Same-route Responses and Chat items replay in event order. A complete Chat assistant item or Responses message/function-call item suppresses the duplicate generic assistant message.
- Reasoning-only partial state does not suppress neutral assistant fallback. Incompatible provider/model/protocol state is excluded and neutral assistant history remains for route changes.
- Tool results and user messages preserve order. The winning compaction shadows both neutral and provider state and injects the same portable summary prefix.
- Existing `derive_messages` remains the v1 compatibility projection; no live dispatch changes until C05.

TDD evidence:

- Red: projected request/input/error types and correlation API did not exist.
- Green: Responses reasoning/function state substitution, complete Chat reasoning/tool state, incompatible-route neutral fallback, missing context, orphan/mismatched state and output ordering.

Primary code:

- `crates/heycode-session/src/request_projection.rs`
- `crates/heycode-session/tests/request_projection.rs`
- `crates/heycode-session/src/lib.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 373 passed, 0 failed across 55 non-empty suites on macOS.

Next boundary:

- `C05` maps one `ProjectedRequest` back to the native draft/header/context and fails any live mutation before network I/O.

## 2026-08-25 — C05: independent request-desync gate

Tracker: `C05` complete.

Outcome:

- Extended `RequestDraft`/`ResolvedCall` with paired catalog timestamp, effective comparison instant, exact-model context capacity and model output maximum.
- Extended C02 options/context with adapter-default provenance and effective instant.
- Added `snapshots_from_resolved_call` to produce validated secret-free durable header/context from the live call before commit.
- Added full projected-input mapping: neutral roles/tool calls/results and provider-state items convert to native `InferenceInput` without loss.
- `verify_resolved_call` compares every header field, prompt hash/text, tool schema, option/default, capacity/catalog field and ordered input against `ProjectedRequest`.
- Success returns non-Clone `VerifiedResolvedCall<'adapter>` borrowing the exact adapter instance; parameterless consumed dispatch prevents a registration swap between verification and stream construction.
- Mismatch errors name only the stable field and never echo prompts, schemas, state, endpoint auth values or credentials.

TDD evidence:

- Red: request context evidence/getters and agent snapshot/verify/verified-dispatch APIs did not exist.
- Green: matching one-shot dispatch; mutated system/tool/input/provider, catalog revision and adapter-default provenance all fail with zero transport calls.

Primary code:

- `crates/heycode-llm/src/inference.rs`
- `crates/heycode-session/src/request.rs`
- `crates/heycode-agent/src/request_invariant.rs`
- `crates/heycode-agent/tests/request_invariant.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 376 passed, 0 failed across 56 non-empty suites on macOS.

Next boundary:

- Provider profile/loop activation must resolve → snapshot/commit → `project_requests` → verify → dispatch through this gate. The legacy v1 compatibility path remains until its selected provider has live catalog/capability evidence.

## 2026-08-25 — PDS01: DeepSeek V4 live catalog and legacy retirement

Tracker: `PDS01` complete.

Outcome:

- Added `heycode-provider-deepseek`, a provider-contribution crate rather than embedding discovery in the CLI or LLM kernel.
- Plugin `catalog-deepseek` injects `models`, `credentials` and `http`, registers one effect-owned `ModelCatalog`, and is mounted after durable cache restoration and before LLM composition.
- Each refresh resolves its credential reference at operation time, preserving environment/keychain/file precedence and observing rotation without recomposition. A DeepSeek-specific custom API base and credential reference flow from current config.
- Added bounded buffered HTTP to the existing shared transport. All status codes return to the owning provider plugin; cancellation and response caps remain transport-owned.
- Authenticated `/models` data is validated as one complete `list` generation. Blank/duplicate/wrong-owner/wrong-kind rows and malformed JSON reject the entire candidate.
- `deepseek-v4-flash` and `deepseek-v4-pro` normalize to the evidenced 1,048,576-token context, 393,216-token output maximum, Preview lifecycle and Supported tool/reasoning/prompt-cache fields. Every other unproven field remains Unknown.
- Maintained tombstones make `deepseek-chat` and `deepseek-reasoner` Retired at 2026-07-24T15:59:00Z with `deepseek-v4-flash` as the ordered replacement. Catalog-backed resolution rejects either old configured id and selects current V4.
- 401/403, 429/5xx, network, cancellation and invalid response shapes map to stable body-free catalog failures. Credentials and provider response bytes never enter diagnostics.

TDD evidence:

- Red: the new crate had no library target; CLI exact inventory lacked `catalog-deepseek`.
- Green: V4 normalization, retirement alternatives, malformed all-or-nothing responses, status redaction, pre-cancellation, operation-time credential rotation, custom base URL and effect teardown all pass.

Live evidence:

- The development host had no `DEEPSEEK_API_KEY`, so successful authenticated discovery is not claimed.
- A safe unauthenticated request to the official `/models` endpoint returned HTTP 401, confirming reachability and the expected auth boundary. A future credentialed live lane remains under QLIVE01.

Primary code:

- `crates/heycode-provider-deepseek/{src/lib.rs,tests/catalog.rs,tests/plugin.rs}`
- `crates/heycode-http/{src/transport.rs,tests/buffered_http.rs}`
- `crates/heycode-cli/{src/lib.rs,tests/composition.rs}`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 385 passed, 0 failed across 59 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `B08` changes every fresh/default DeepSeek selection to a current catalog-backed V4 id and migrates only the exact historical implicit default. `PDS02` then owns thinking controls and effort mapping.

## 2026-08-25 — B08: current DeepSeek default and schema-v2 migration

Tracker: `B08` complete.

Outcome:

- Fresh `Config`, `DeepSeekProvider`, the setup picker/writer and the DeepSeek catalog now agree on canonical `deepseek-v4-flash`. A cross-crate CLI test prevents those four owners from drifting.
- Advanced the persisted configuration schema from v1 to v2. Unversioned/v1 remain readable and migration-plannable; newer schemas still fail before partial loading.
- Added typed `ReplaceRetiredDeepSeekDefault { from, to }` preview evidence and redacted startup notices.
- Automatic home migration rewrites `deepseek-chat` only when the older document matches the exact setup-generated root, LLM and historical tool-default fingerprint. It preserves the model value's inline decoration/comment, creates a byte-exact `.v1.bak`, source-CAS checks and atomically commits.
- Customized v1 documents, `deepseek-reasoner`, OpenRouter routes and current-schema pins retain exact user intent. They can receive catalog retirement rejection/advice instead of a silent rewrite.
- The real binary migration e2e proves schema v1 → v2, V4 replacement, exact backup and visible safe notice before normal composition.

Regression caught during TDD:

- The first implementation changed every older `provider=deepseek, model=deepseek-chat` document. The customized-v1 regression test failed, showing that string equality was not provenance. Migration now requires the setup document fingerprint (GOTCHAS #56).

Primary code:

- `crates/heycode-config/src/{lib,migration}.rs`
- `crates/heycode-config/tests/migrations.rs`
- `crates/heycode-llm/src/deepseek.rs`
- `crates/heycode-cli/src/main.rs`
- `crates/heycode-cli/tests/{composition,e2e}.rs`
- `crates/heycode-provider-deepseek/tests/catalog.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 390 passed, 0 failed across 59 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `PDS02` owns DeepSeek thinking on/off and exact high/max effort resolution, including removal of unsupported sampling fields in thinking mode. `PDS03` then enforces reasoning-state replay across tool turns.

## 2026-08-25 — PDS02: DeepSeek thinking toggle and effort resolution

Tracker: `PDS02` complete.

Outcome:

- Added reusable `ChatThinkingConfig` to route configuration. It owns an explicit disabled id, a complete canonical-to-wire effort map and enabled-mode omission policies; shared Chat logic never branches on provider name.
- Adapter construction rejects a disabled id outside the accepted choices, incomplete maps, duplicate/unaccepted canonical ids and thinking profiles without an effort dialect.
- DeepSeek exposes exact `none`, `high`, `max`, defaulting to high only when the selected model descriptor proves reasoning support. `none` emits `thinking.type=disabled` with no effort; high/max emit enabled plus scalar `reasoning_effort`.
- Enabled thinking removes `temperature` during request resolution, so `ResolvedCall`, C02 snapshots and wire bytes agree. Disabled thinking retains the explicit sampling value.
- Thinking requests retain tool schemas but omit generic `tool_choice` and `parallel_tool_calls`, which are not proven compatible for this route.
- Exact unknown custom models preserve P01 advisory behavior: absent an explicit reasoning request, they do not inherit V4 thinking defaults or capability claims.

Regression caught during implementation:

- Initial serialization-only temperature removal left the resolved/durable option as `Some(0.7)` even though the provider body omitted it. A strengthened test failed, and normalization moved to `resolve` (GOTCHAS #57).
- An initial attempt made adapter-default reasoning reject all Unknown model descriptors, breaking the existing unlisted-model contract. The default now materializes only with proven model capability.

Primary code:

- `crates/heycode-llm/src/{chat,deepseek,inference,lib}.rs`
- `crates/heycode-llm/tests/{deepseek_thinking,chat_protocol}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 395 passed, 0 failed across 60 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `PDS03` validates that every thinking-mode DeepSeek assistant tool-call state includes complete `reasoning_content` and survives projection into the next request. Missing state must fail before transport.

## 2026-08-25 — PDS03: required DeepSeek reasoning state across tool turns

Tracker: `PDS03` complete.

Outcome:

- Extended `ChatThinkingConfig` with a route-owned requirement for tool-call `reasoning_content`; no provider-name conditional entered shared Chat code.
- After common resolution determines the effective thinking mode, enabled calls inspect their exact chronological inputs. Lossless Chat assistant state with tool calls requires nonempty string `reasoning_content`.
- A neutral assistant tool-call message cannot satisfy the requirement because it cannot represent provider reasoning. Missing/malformed state returns safe `InvalidRequest { field: "provider_state" }` before the transport stream is constructed.
- Complete state replays byte-for-byte into the next Chat request beside the matching tool result. Disabled thinking accepts non-reasoning tool history.
- The response parser applies the symmetric policy after fully accumulating a choice. An enabled-thinking tool-call response with no reasoning returns a terminal protocol error and emits neither `ProviderState` nor successful `Finish`.

TDD evidence:

- Red: both a state item with tool calls but no reasoning and a neutral assistant fallback resolved successfully; a malformed provider response produced valid continuation state.
- Green: complete replay, missing-state/neutral pre-dispatch refusal, disabled-mode compatibility and response-side no-state/no-finish failure.

Primary code:

- `crates/heycode-llm/src/{chat,deepseek}.rs`
- `crates/heycode-llm/tests/deepseek_thinking.rs`
- Existing `crates/heycode-session/tests/request_projection.rs` continues to prove C04 same-route lossless Chat-state substitution.

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 398 passed, 0 failed across 60 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- Recompute the dependency-ready P0/P1 frontier. DeepSeek's remaining PDS04 needs P05 (Anthropic); PDS05 is P3. Native provider-loop activation still requires N01/C06/C07-adjacent orchestration, while P08/P09 harden every provider boundary.

## 2026-08-25 — Q01: shared real-composition test harness

Tracker: `Q01` complete.

Outcome:

- Added public `heycode_cli::testing::RealCompositionHarness`, the shared downstream entry point for product-level tests.
- Each harness owns isolated workspace, settings, credentials, sessions and durable catalog paths plus mutable compiled-default config.
- `compose()` consumes the inputs and invokes the production `compose_world` factory table, profile resolution and plugin loader. Only inference is replaced with the sanctioned `FakeProvider`; every default product plugin is real.
- The returned `ComposedTestWorld` owns `Context` and `TempDir`. Explicit shutdown and Drop unwind all plugin effects before removing the filesystem root.
- The pre-existing exact plugin/service/tool/command/contribution inventory audit now uses this shared harness, so later default plugins automatically join the common real-composition proof.

TDD evidence:

- Red: the integration test could not import `heycode_cli::testing`.
- Green: the shared harness boots all 26 current runtime plugin instances through the real loader, exposes only root-contained paths and pairs every applied plugin with a descriptor.

Primary code:

- `crates/heycode-cli/src/testing.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{real_composition_harness,composition}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 399 passed, 0 failed across 61 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `Q02` builds the reusable provider conformance fixture runner on top of raw HTTP/SSE and adapter contracts so fragmentation/failure matrices stop being hand-written per protocol.

## 2026-08-25 — Q02: reusable provider conformance fixture runner

Tracker: `Q02` complete.

Outcome:

- Added `heycode_llm::testing` raw-SSE conformance types: validated safe fixture/case ids, named raw chunk scripts, explicit terminal transport failures, decoder-backed transport and ordered async runner results.
- `SseConformanceFixture::fragmentation_cases` generates whole-body, bytewise and every two-fragment split (`N + 1` cases for `N` bytes) deterministically.
- `SseFixtureTransport` passes chunks through the production `SseDecoder`; adapters receive the same `SseEvent` boundary as production. Each isolated case records exact transport call count.
- Terminal failure cases do not call clean-EOF `finish`, preventing partial frames from being published before a disconnect error.
- The same runner proves Chat Completions and Responses normalized outputs invariant across every raw split. Network disconnect and invalid UTF-8 cases terminate with one error and no synthetic Finish.
- Fixture source/version/capture evidence remains scoped to CAT08/Q08; retry/error taxonomy extensions remain P08.

TDD evidence:

- Red: conformance fixture/runner imports did not exist.
- Green: two protocol adapters share the generated fragmentation matrix; reusable failure scripts, call counts and boundary validation pass.

Primary code:

- `crates/heycode-llm/src/testing/conformance.rs`
- `crates/heycode-llm/src/testing.rs`
- `crates/heycode-llm/tests/provider_conformance.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 403 passed, 0 failed across 62 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `QSEC01` is the next tracker-ordered open P0: persist an evidence-backed repository threat model and security policy before expanding executable/MCP/plugin surfaces. `DOC01` then audits the plan-link graph.

## 2026-08-25 — DOC01: authoritative program-link freshness

Tracker: `DOC01` complete.

Outcome:

- Added a path-resolving documentation contract over root README, current STATUS, the historical completed TASKS backlog and FEATURES.
- Every document must directly link both the engineering master plan and authoritative dependency tracker using a path relative to that document.
- Every target is resolved on disk and must be a regular file; a matching label or directory-only link does not satisfy the test.
- README now links `docs/engineering/TASKS.md` directly rather than requiring readers/automation to infer the tracker through the directory/master page.

TDD evidence:

- Red: README linked the master program but not the active dependency tracker.
- Green: all four required documents contain both direct links and every path resolves.

Primary code:

- `crates/heycode-cli/tests/documentation_program_links.rs`
- `README.md`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 404 passed, 0 failed across 63 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `QSEC01` remains active: the repository threat-model body is drafted, but the root `SECURITY.md` candidate cannot be written until the required explicit owner approval arrives. Continue independent work with K04 plugin contribution inventory rather than spin on that gate.

## 2026-08-25 — K04: exact plugin contribution inventory

Tracker: `K04` complete.

Outcome:

- Added exact `ContributionKind` namespaces that distinguish services, inference providers, model catalogs, persistence/settings/credential providers, authorization flows, tools, commands, prompt sections, interception seams/layers, UI rows and external processes.
- `Context` owns a shared `PluginInventory` and marks the plugin currently applying. Successful `provide` calls attribute services automatically; `Plugin::inventory` declares static rows; `Context::contribute` accepts dynamic rows only during apply.
- Duplicate exact kind/name claims fail composition naming both owners. Built-in exact rows are checked against the plugin descriptor's broad contribution family before its descriptor is committed. Unclassified compatibility/test plugins remain exempt.
- Default plugins now declare their exact rows. MCP contributes configured process ids statically and discovered qualified tool ids dynamically after handshake.
- `/plugins [verbose]` is a human-only command backed by the shared live snapshot. Verbose output groups exact rows under runtime id/version/source.
- The exact default-world audit compares inventory services/tools/commands/prompt sections with their live registries, preventing missing attribution. Additional tests pin settings/credential/auth/catalog/interception/UI rows and dynamic MCP rows.

Regression caught during full gates:

- Duplicate LLM provider names now fail earlier as an exact `inference_provider` collision, requiring the old generic-duplicate assertion to adopt the stronger error.
- Memory/file settings plugins now correctly declare both Service and Provider broad families; stale descriptor tests caught the expanded truth.

Primary code:

- `crates/heycode-core/src/{inventory,context,plugin,descriptor,error,lib}.rs`
- exact inventory declarations across tools/prompt/LLM/settings/credentials/auth/catalog/skills/MCP/subagent/plan/TUI plugins
- `crates/heycode-agent/src/{commands,plugin}.rs`
- `crates/heycode-core/tests/contribution_inventory.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 409 passed, 0 failed across 65 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `K05` adds plugin scopes and deterministic precedence without allowing silent service replacement. `K06` then records profile/config source metadata in the same effective tree used by future doctor and UI surfaces.

## 2026-08-25 — K05: deterministic plugin scopes and precedence

Tracker: `K05` complete.

Outcome:

- Added `PluginScope::{BuiltIn,User,Project,LocalProject,Session,Managed}` with stable diagnostic ids and fixed low-to-high precedence.
- Added `PluginDirective`, unique `PluginScopeLayer` and `resolve_scoped_plugins`. Caller layer order cannot alter output; duplicate base ids, scopes, per-layer ids and malformed plugin ids fail loud.
- Existing effective rows retain dependency-sensitive position while adopting a higher winning scope. New rows and rows re-enabled after removal append at the enabling directive's position. Managed directives apply last.
- Added `ScopedPlugin`/`compose_scoped` and `PluginFactories::build_scoped`, consuming each winning factory once through the same transactional composition checks.
- `AppliedPlugin` records implementation descriptor/source separately from activation scope. Context/inventory expose aligned runtime scopes, and `/plugins` includes them. Ordinary `compose` remains a BuiltIn-scoped compatibility path.
- Scope selection never authorizes service/exact contribution shadowing; normal collisions and injections still fail.

TDD evidence:

- Red: no scope, layer, resolver, scoped factory build or runtime metadata types existed.
- Green: shuffled user/project/local/session/managed layers produce identical order/winners; duplicate inputs fail; resolved scopes reach real runtime inventory; default production composition reports BuiltIn for every row.

Primary code:

- `crates/heycode-core/src/{descriptor,plugin,context,inventory,lib}.rs`
- `crates/heycode-config/src/scopes.rs`
- `crates/heycode-config/src/lib.rs`
- `crates/heycode-core/tests/contribution_inventory.rs`
- `crates/heycode-config/tests/plugin_scopes.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 413 passed, 0 failed across 66 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `K06` defines versioned profile documents and config-source metadata, producing an inspectable effective tree. `K07` later selects named files and applies those layers in the CLI.

## 2026-08-25 — K06: versioned profile schema and source-aware effective tree

Tracker: `K06` complete.

Outcome:

- Added strict standalone profile schema v1, independent of the root config schema (v2 at this boundary; now v3). Documents contain exact schema version, optional kebab-case name and ordered `[[plugins]] { id, enabled }` rows.
- Unknown fields, unsupported versions, malformed names/ids and duplicate rows fail loud. Enabled defaults true only when the row exists and omits the field.
- Added trusted `ProfileSource` variants for built-in, user/project/local files, session overrides and managed policy. Source metadata is never accepted from profile bytes; `ProfileLayer` enforces source/scope match and safe path/label shape.
- `resolve_profile_tree` retains the built-in layer plus every precedence-ordered overlay/source, every mentioned plugin including disabled rows, complete decision history, last source/scope/state and exact K05-derived enabled composition order.
- K05 and K06 cannot drift silently: enabled order/winning scopes come from `resolve_scoped_plugins`; the profile tree only attaches source/history metadata.

TDD evidence:

- Red: profile schema/source/layer/tree APIs did not exist.
- Green: user/project files preserve every decision and correct winning source; strict version/unknown/malformed/duplicate and source-scope mismatch cases fail.

Primary code:

- `crates/heycode-config/src/profiles.rs`
- `crates/heycode-config/src/{lib,scopes}.rs`
- `crates/heycode-config/tests/profile_metadata.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 415 passed, 0 failed across 67 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `K07` discovers/selects `$HEYCODE_HOME/profiles/<name>.toml`, exposes `--profile`, and composes that same source-aware effective tree. Selection must not fork another resolver or permit path traversal.

## 2026-08-25 — K07: safe named profile discovery and CLI selection

Tracker: `K07` complete.

Outcome:

- Added `NamedProfileStore` rooted at fixed `$HEYCODE_HOME/profiles`, shared by CLI and future picker consumers. Listing is sorted and validates the same documents returned by load.
- Named profile ids are lowercase kebab-case stems. Store/root/file symlinks or wrong types, traversal/malformed names, files over 1 MiB, strict schema failures and embedded-name mismatch fail loud.
- Named files attach trusted `ProfileSource::NamedProfile` at User scope; bytes cannot forge source metadata.
- Added `--profile <name>` parsing/help with missing/duplicate rejection and an explicit setup incompatibility. Setup edits base connection settings rather than overlays.
- `WorldOptions` carries already-discovered profile layers. `compose_world` resolves K06/K05, consumes scoped factories and calls `compose_scoped`; ACP and normal worlds use the same layer vector.
- A named profile cannot combine with legacy complete `[profile] plugins`; ambiguous replacement semantics fail instead of guessing.
- Real-composition tests prove the picker-loaded layer and world share one path, optional MCP disablement and User-scope re-selection reach runtime inventory. A real binary named-profile fake run passes.

TDD evidence:

- Red: named store and CLI profile field did not exist.
- Green: sorted list/load, path/symlink/cap/name failures, CLI parsing, picker/world parity, ambiguity refusal and real binary selection.

Primary code:

- `crates/heycode-config/src/{profile_store,profiles,lib}.rs`
- `crates/heycode-config/tests/named_profiles.rs`
- `crates/heycode-cli/src/{main,lib,testing}.rs`
- `crates/heycode-cli/tests/{composition,e2e}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 422 passed, 0 failed across 68 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `K08` exposes human and JSON `heycode doctor --composition` from profile layers, scoped factories, descriptors, injections and exact inventory. Failures must be inspectable without leaking secrets.

## 2026-08-25 — K08: side-effect-free production composition doctor

Tracker: `K08` complete.

Outcome:

- Added static `Plugin::provides()` declarations and a serializable `CompositionReport` in core. Dry inspection checks descriptor/name drift, duplicate plugin ids, ordered injections, blocked dependency cascades, service collisions, exact contribution collisions, invalid exact names and broad-family mismatch without applying a plugin.
- Extracted `resolve_world_plugins` as the one production graph builder used by both `compose_world` and `inspect_world`; profile selection, scope resolution, availability filtering and factory construction therefore cannot drift between doctor and runtime.
- Added `heycode doctor --composition` and `--json`. Healthy reports exit 0; invalid profiles return a complete safe report and exit 1. The command does not resolve credentials, open sessions/settings, start watchers or MCP children, or dispatch network traffic.
- Made `agent-options` a real heycode-agent-owned plugin/service and an explicit `agent` injection. Removed the agent's conditional fallback so planned and live graphs agree. Updated all real and cross-crate composition harnesses to mount that prerequisite.
- Added root config schema v3 migration for older custom complete profiles that relied on the former implicit fallback. It preserves every chosen plugin and decoration, inserts only `agent-options` immediately before `agent`, emits a typed redacted notice, retains byte-exact backup/CAS/idempotence guarantees and leaves explicit/project files pending.

TDD evidence:

- Red: no static provided-service plan, dry graph report or doctor mode existed; composition itself was the only validator and could create state.
- Green: core reports missing dependencies and service collisions without invoking `apply`; real default doctor JSON is healthy and leaves session/settings/credential paths absent; a broken named profile names the blocked plugin/dependency and exits nonzero.
- Boundary regressions: a real binary custom-profile migration and three skills composition worlds exposed the newly explicit prerequisite. Schema-v3/unit/e2e migration coverage and shared test-world fixes now pin compatibility without weakening fail-loud composition.

Primary code:

- `crates/heycode-core/src/{inspection,plugin,lib}.rs`
- `crates/heycode-core/tests/composition_inspection.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-cli/tests/{composition,composition_doctor,e2e}.rs`
- `crates/heycode-agent/src/{lib,plugin}.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 428 passed, 0 failed across 70 non-empty suites on macOS.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `S11` introduces a unified plugin-contributed doctor registry and stable redacted result schema. K08 remains its side-effect-free composition check; activation-safe settings/credential/provider/runtime checks share one human/JSON result plane.

## 2026-08-25 — S11: unified plugin-contributed doctor plane

Tracker: `S11` complete.

Outcome:

- Added the core-only `heycode-doctor` Service Definition crate and service key `doctor`. `DoctorRegistry` registers async checks as context effects, refuses duplicate validated ids, snapshots deterministic order and removes every contribution on shutdown.
- Added schema-v1 `DoctorReport` with pass/warning/failure/skipped counts, stable result codes, nonzero health semantics and one source for human/JSON rendering. Warning remains healthy; failure or cancellation/skipping is unhealthy.
- The registry threads one `CancellationToken`, drops a running future on cancellation, skips remaining checks and contains panics or invalid outcomes as fixed safe failure codes so one plugin cannot starve the report.
- Outcome summaries and repairs accept compile-time-static text only. Runtime evidence is a closed typed enum, currently K08's already-redacted `CompositionReport`; there is no arbitrary string/JSON evidence escape hatch.
- Added the broad `Diagnostic` descriptor family and exact `doctor_check` inventory namespace. The default world mounts `doctor`, `doctor-settings` and `doctor-credentials`; exact service/plugin/descriptor/inventory/migration fixtures were updated together.
- Settings owns a writer-availability check. Credentials owns a provider-count check that never inspects or resolves a reference. Both are separately disposable/attributed contributions.
- Added `heycode doctor [--json]`. It runs settings, credentials and K08 composition in a restricted diagnostic world with a non-watching settings reader and non-resolving environment/keychain registrations. It creates no session, settings/credential file, catalog/network client or child process. `doctor --composition` retains the stricter zero-apply K08 view.

TDD evidence:

- Red: no doctor crate/service/result/check namespace existed; the CLI rejected `doctor` without `--composition`.
- Green: registry ordering, duplicate refusal, lifecycle disposal, cancellation, id/code validation, stable JSON/human projection and warning semantics; settings/credentials owned check tests; real healthy/broken-profile CLI tests.
- A process environment canary secret is configured during the real JSON doctor test and is absent from output. The same test proves session/settings/credential paths remain absent.

Primary code:

- `crates/heycode-doctor/src/{lib,error,model,registry,plugin}.rs`
- `crates/heycode-doctor/tests/registry.rs`
- `crates/heycode-settings/src/doctor.rs`
- `crates/heycode-settings/tests/doctor.rs`
- `crates/heycode-credentials/src/{doctor,service}.rs`
- `crates/heycode-credentials/tests/doctor.rs`
- `crates/heycode-core/src/{descriptor,inventory}.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-cli/tests/{composition,composition_doctor,plugin_inventory,real_composition_harness}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 435 passed, 0 failed across 73 non-empty suites on macOS.
- Real-binary `--fake doctor --json` and `--fake doctor --composition --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- Continue with the next dependency-ready P1 from `TASKS.md`; provider/MCP/sandbox/runtime/session doctor checks remain with their owning feature tasks and must extend this registry without adding arbitrary runtime-text evidence.

## 2026-08-25 — B09: service-backed terminal provider/model setup

Tracker: `B09` complete.

Outcome:

- Removed the binary `PROVIDERS` table. `ProviderRegistry::profiles()` now projects provider-owned registry name, safe descriptor/display, default model and optional non-secret credential reference. `SetupCatalog` rejects identity/default drift and reads catalog availability from the composed `models` service.
- Added deterministic provider number/id and model number/id resolvers. Catalog generations are id-sorted and effectively retired rows are excluded. Live/fresh/stale provenance and warnings remain explicit.
- A missing/failed catalog falls back visibly to the provider-owned default. Custom model ids are accepted only in that unproven fallback; a live catalog rejects absent ids. OpenRouter correctly remains fallback until POR02 rather than pretending to have a catalog.
- Added restricted `SetupWorld`: non-watching settings, credential providers, shared HTTP, model registry/cache, real DeepSeek catalog and metadata-only DeepSeek/OpenRouter providers. It cannot dispatch inference and composes no session, agent, tools, TUI or MCP process.
- Converted `run_setup_wizard` to consume `SetupCatalog`. Provider defaults follow the existing configured provider when present, changing providers clears stale base-url/reference overrides, credential references come from provider profiles, model rows come from the catalog and three invalid key attempts now fail instead of continuing unconfigured.
- Setup persistence now writes the selected approval policy and credential reference, serializes TOML values rather than interpolating text, atomically replaces a regular file, refuses symlinks and enforces `0700` parent/`0600` config modes on Unix.

TDD evidence:

- Red: no `SetupCatalog`, dynamic provider/model resolvers or restricted setup world existed; the terminal wizard indexed a two-row constant.
- Green: injected fake provider/catalog services prove dynamic sorting, selectable/retired filtering, recommendation, live-catalog refusal and visible custom fallback. The real restricted world projects exact provider-owned DeepSeek/OpenRouter defaults/references and no session/settings/credential file side effects.
- Config tests prove quote/newline round-trip, current schema/no frozen profile, approval/reference persistence, owner modes, atomic replacement and symlink refusal.

Manual verification note:

- A real PTY probe reached the dynamic provider, visible OpenRouter no-catalog fallback, model and approval pages. It is not counted as end-to-end acceptance because the harness was unstable at final input.
- An earlier masked-input PTY attempt wrote a known dummy OpenRouter entry to the process-global macOS Keychain even under a temporary `HEYCODE_HOME`. Exact-value cleanup is pending Mac unlock/authorization; the safe operational warning is persisted in STATUS and GOTCHAS #69. No further live-keychain/provider verification was used.

Primary code:

- `crates/heycode-cli/src/{setup,main,lib}.rs`
- `crates/heycode-cli/tests/setup_catalog.rs`
- `crates/heycode-llm/src/{provider,registry,deepseek,openrouter,lib}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 439 passed, 0 failed across 74 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `B10` projects typed config migration and S11 health state through one redacted JSON startup/diagnostic surface without exposing raw document bytes or secret values.

## 2026-08-25 — B10: redacted migration evidence in unified doctor

Tracker: `B10` complete.

Outcome:

- Added the config-owned `doctor-config` plugin and exact `doctor_check:config-migration` contribution to the default profile. `WorldOptions` carries the selected startup notice into normal, ACP and restricted doctor worlds; tests pass `None` explicitly rather than inventing state.
- Extended schema-v1 doctor evidence with closed config version/disposition/change types. Evidence includes source/backup paths, prior/target schema and semantic changes only. `ConfigMigrationPlan` original/rendered bytes remain private and structurally cannot enter the report.
- Current schema reports pass. Safely applied home migration reports pass with backup evidence. Explicit/project-owned pending migration reports warning with repair guidance; warnings remain healthy but visible.
- Human rendering escapes path control characters. JSON uses the same typed result and serde escaping. There is no generic raw/debug/JSON field.
- Startup loading now retains the migration notice after printing its existing human notice. The unified `heycode doctor [--json]` includes it; `doctor --composition` remains the zero-apply graph-only K08 contract.

TDD evidence:

- Red: startup discarded migration state before composition and no config doctor contribution/evidence types existed.
- Green: current and pending plugin mappings, exact inventory attribution and stable disposition/change JSON.
- A real explicit schema-v2 config contains an unknown extension `api_key` canary. Doctor returns a typed pending warning, leaves the file byte-identical, and the canary is absent from stdout, stderr and JSON evidence.

Primary code:

- `crates/heycode-config/src/{migration_doctor,lib}.rs`
- `crates/heycode-config/tests/migration_doctor.rs`
- `crates/heycode-doctor/src/{model,lib}.rs`
- `crates/heycode-cli/src/{lib,main,testing}.rs`
- `crates/heycode-cli/tests/{composition,composition_doctor,e2e,plugin_inventory,real_composition_harness}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 441 passed, 0 failed across 75 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `S10` adds an effect-owned command-backed credential provider with strict argv execution, timeout/empty-output handling, operation-time rotation and secret-safe diagnostics.

## 2026-08-25 — S10: bounded exact-argv command credential provider

Tracker: `S10` complete.

Outcome:

- Added `heycode-credentials-command`, a precedence-5 read-only provider between environment and keychain. Immutable specs bind one validated credential reference to one exact executable/argv vector; the provider never adds a shell.
- Specs cap argv count/component bytes, reject NUL/blank programs, enforce nonzero deadlines up to 60 seconds and copy only explicitly allowlisted, validated environment names into an otherwise empty child environment.
- Every resolve executes again, so external rotation reaches the next operation. Stdout is drained concurrently and capped at 64 KiB; terminal CR/LF is removed and exactly one non-empty UTF-8 line becomes `CredentialSecret`.
- A named private worker creates a current-thread Tokio runtime, avoiding nested-runtime panic when the synchronous credential seam is called inside the product runtime. Timeout kills and reaps the direct child; pipe settlement is bounded.
- Spawn/runtime/worker/timeout/empty/nonzero/oversized/read/invalid-output failures are fixed messages. Argv, stdout, stderr and OS error text never enter errors or the provider's redacted `Debug`.
- The default product mounts `credentials-command` with zero specs and exact inventory attribution. This makes the plugin lifecycle real without activating executable project config before K12 trust and E02/E04 process-tree/sandbox routing.

TDD evidence:

- Red: no command credential crate/provider/plugin existed.
- Green: a backing file changes from first to second secret between resolves inside an active Tokio runtime; descriptor source is `command`; context shutdown removes the provider; duplicate references fail.
- Direct sleep times out; empty, nonzero-stderr-canary and oversized-stdout cases map to their fixed classes. Canary values are absent from every error and `Debug`.

Primary code:

- `crates/heycode-credentials-command/src/{lib,error,spec,provider,plugin}.rs`
- `crates/heycode-credentials-command/tests/command_provider.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,real_composition_harness}.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 444 passed, 0 failed across 76 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U03` defines the effect-owned UI slot/panel/dialog/status contribution registry so product surfaces and future external plugins can extend the TUI without direct renderer imports or hardcoded panel lists.

## 2026-08-25 — U03: typed effect-owned UI contribution registry

Tracker: `U03` complete.

Outcome:

- Added core-only `heycode-ui` and service `ui`. `UiContributionDescriptor` validates `panel|dialog|status`, dotted/kebab id, trimmed control-free title and signed priority.
- `UiRegistry` stores typed opaque `Arc<T>` handles. Universal metadata stays kernel-owned while each capability plugin defines its actual panel/action contract; wrong-type lookup returns `None`, poisoned state fails loud.
- Registration is allowed during plugin apply, publishes exact `ui_slot` inventory first for attributed collision errors, commits live state second and installs a token-checked context disposer. Rollback/shutdown remove every handle.
- Same id in different slots is legal. Same slot/id fails. Metadata snapshots sort slot, descending priority and id, independent of hash/registration timing.
- Added default plugin `ui`, service-key registry row and production factory/order entry. The real `tui` plugin injects it and dynamically contributes `panel:transcript`, `dialog:approval` and `status:session` alongside its existing complete-UI row.
- Root config schema v4 migrates older custom complete profiles containing `tui` but missing `ui`, inserting only `ui` immediately before `tui`. The existing v2→v3 `agent-options` repair remains; real-binary custom-profile migration pins both.

TDD evidence:

- Red: no UI service, slot vocabulary, typed registry or lifecycle existed.
- Green: panel/dialog/status registration, cross-slot same-id, descending priority, typed/wrong-type lookup, dynamic inventory attribution, invalid descriptor rejection, same-slot collision and post-shutdown empty snapshot.
- Real composition proves the `ui` service, plugin descriptor/order, three TUI slots, `/plugins verbose` rows and disposal. The first full gate exposed the custom-profile prerequisite regression; schema-v4 unit/e2e coverage now prevents recurrence.

Primary code:

- `crates/heycode-ui/src/{lib,error,model,registry,plugin}.rs`
- `crates/heycode-ui/tests/registry.rs`
- `crates/heycode-tui/src/plugin.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,real_composition_harness,e2e}.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 447 passed, 0 failed across 77 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `CMD01` extends command descriptors with arguments, execution timing, availability and source metadata. U04 then renders/searches those registry rows for `/` and Ctrl+P instead of hardcoding commands in the TUI.

## 2026-08-25 — CMD01: validated command discovery metadata

Tracker: `CMD01` complete.

Outcome:

- Added validated `CommandDescriptor`, `CommandArgument`, `CommandTiming`, `CommandSource`, `CommandAvailability` and `CommandCatalogEntry` vocabulary in heycode-agent.
- Arguments are ordered/unique; required precedes optional; variadic must be final. Descriptors expose generated slash synopsis, one-line description, `immediate|queued|interrupting|model_scheduling`, owning plugin and optional shortcut.
- `CommandRegistry::{catalog,help_lines,names,get}` includes built-in and late plugin commands in deterministic registration order and now fails on poisoned late state instead of silently dropping commands. Duplicate ids fail before publication.
- Unavailable commands remain catalog rows with a visible reason. Availability is dynamic per command object and cannot be reconstructed by the palette.
- Refactored all ten commands: built-in help/model/provider/plugins/compact/title/quit, skills/skill and plan. `/help` renders descriptor synopsis/description from the live registry.
- TUI dispatch handles registry failure distinctly from unknown command. Real composition pins every synopsis/timing/source and joins each descriptor source to K04 exact command inventory ownership.

TDD evidence:

- Red: command objects exposed only `name/help`; no structured args/timing/source/shortcut/availability/catalog existed.
- Green: metadata validation, required/optional/variadic synopsis, shortcut, unavailable reason, catalog retention/order and duplicate rejection.
- Existing command behavior suites, late `/help`, skill/model scheduling, plan, title, compact, TUI and exact production inventory all pass after the registry API migration.

Primary code:

- `crates/heycode-agent/src/{command_descriptor,commands,plan,plugin,lib}.rs`
- `crates/heycode-agent/tests/{command_metadata,law,compact,plan,title}.rs`
- `crates/heycode-skills/src/lib.rs`
- `crates/heycode-skills/tests/skills.rs`
- `crates/heycode-tui/src/app.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory}.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 449 passed, 0 failed across 78 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U04` implements the searchable `/` and Ctrl+P palette over `CommandRegistry::catalog`, including fuzzy ranking, source/timing/shortcut/availability rendering and selection back into the composer.

## 2026-08-25 — U04: searchable slash and Ctrl+P command palette

Tracker: `U04` complete.

Outcome:

- Added pure `command_palette::filter_commands` over `CommandCatalogEntry`; the TUI keeps no command metadata table.
- Ranking supports exact, prefix, substring, subsequence and edit-distance≤2 matching across id, description and source plugin. Id matches outrank description/source and stable ties preserve registry order.
- Empty-composer `/` and Ctrl+P open the same centered modal from a fresh catalog snapshot. Ctrl+P preserves existing composer text; `/` does not first insert a slash.
- Bounded typing/paste and Backspace refilter; arrows wrap; Esc closes. Enter inserts `/id` plus a trailing space only when arguments exist. It does not insert placeholder syntax or execute immediately.
- Unavailable commands stay visible with their reason and cannot mutate the composer. Rows render synopsis, description/reason, source, timing and shortcut.
- Masked secret, onboarding and approval requests close/preempt the palette; approval keys can never target a visually hidden security dialog. U11 remains the owner of active-turn timing enforcement.
- `run_interactive` attaches the live registry to AppState, so late plan/skills rows appear without renderer changes.

TDD evidence:

- Red: no palette module/state/render path or `/`/Ctrl+P handling existed.
- Green: exact/prefix/subsequence/typo/description/source ranking and unavailable retention.
- TestBackend/key tests cover open, rendered metadata, bounded query filtering, selected insertion, Ctrl+P preservation, Esc close, unavailable refusal and approval preemption. All prior TUI interaction/render tests remain green.

Primary code:

- `crates/heycode-tui/src/{command_palette,app,render,lib}.rs`
- `crates/heycode-tui/tests/command_palette.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 452 passed, 0 failed across 79 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U05` adds the welcome/status card over effective runtime/model/permission/workspace state and S11 health, using UI contributions rather than embedding another startup data source in the renderer.

## 2026-08-25 — U05: effective-state welcome and persistent status

Tracker: `U05` complete.

Outcome:

- Added `ApprovalPolicyKind::{Auto,Ask,Deny,Custom}` and policy introspection. Auto/Deny/Interactive implementations report their actual composed behavior; custom policies default visibly to custom.
- Added `Agent::runtime_id()` as the effective runtime source (`native` until R01), plus existing live selection and approval accessors. Presentation no longer infers these fields from config.
- Added `WelcomeStatusView`/`WelcomeHealth` to AppState. The initial empty transcript renders a rounded card with runtime, provider/model, approval, canonical workspace and health.
- `run_interactive` attaches the composed doctor registry and starts one async health run after first paint. The operation owns one cancellation token with a drop guard; TUI exit cannot detach future network/process checks.
- Health transitions checking → healthy/warning/unhealthy/unavailable with pass/warn/fail/skipped counts. Missing/failed doctor is explicit unavailable, never optimistic green.
- The welcome card yields once transcript content exists. Provider/model changes refresh effective route; provider/model, approval and health persist in the compact status bar.
- CLI passes the live `DoctorRegistry` through LoopDeps; no duplicate health/config data source was introduced.

TDD evidence:

- Red: approval policies had no effective identity, AppState had no welcome/health model and LoopDeps did not carry doctor.
- Green: built-in policy kind mapping; TestBackend checking/healthy-warning/unhealthy/unavailable frames; exact runtime/route/permission/workspace strings; card hides on first user event while conversation renders.
- Doctor-report projection is exercised with a real schema-v1 report, and all existing palette/status/onboarding/approval frames remain green.

Primary code:

- `crates/heycode-agent/src/{approval,interactive_approval,agent,lib}.rs`
- `crates/heycode-agent/tests/approval_status.rs`
- `crates/heycode-tui/src/{app,render}.rs`
- `crates/heycode-tui/tests/welcome_status.rs`
- `crates/heycode-cli/src/main.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 455 passed, 0 failed across 81 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U07` builds a fuzzy live model picker over `CatalogRegistry`, lifecycle/capability filters and stale/refresh evidence; selection must update the effective route without inventing support.

## 2026-08-25 — U07: async live model picker

Tracker: `U07` complete.

Outcome:

- `/model` without arguments now emits typed `ModelPickerRequested { provider, current_model }`; explicit ids retain the compatibility setter path.
- Added pure `model_picker::filter_models` over `CatalogSnapshot::filter_models`. Default Selectable excludes effective retirement; Tab cycles Stable, explicit Tools support and explicit Reasoning support. Unknown stays a badge state and never satisfies supported filters.
- Fuzzy ranking searches provider model id, display name and aliases with the same exact/prefix/substring/subsequence/typo behavior as command discovery.
- Added loading/ready/error AppState with provider/current/query/filter/ranked selection. Catalog success retains live/fresh-cache/stale-fallback provenance and a compact safe warning; failures remain visible without fake rows.
- The TUI event loop owns asynchronous cache-aware/forced refresh waits. Ctrl+R cancels/aborts the prior caller wait and starts Force; Esc/selection cancel the wait. A parent cancellation drop guard settles waits on TUI exit while CAT02's shared source refresh remains registry-owned.
- The modal renders current model, lifecycle and tri-state tools/reasoning badges, fuzzy query, filter, source/warning and controls. Only a filtered catalog row can be selected; retired/free-text ids cannot.
- Enter hands the proven model id back to the loop, updates live Agent selection and effective welcome/status route. Persistence and durable switch/provider-state policy remain CMD02/C14.
- CLI passes the live `CatalogRegistry` through LoopDeps; no TUI-side model table or provider request implementation exists.

TDD evidence:

- Red: no typed request, model picker state/filter/ranker/render path, catalog LoopDeps or refresh task existed.
- Green: retirement exclusion; stable/tools/reasoning exact-evidence filters; alias search; stale warning/source; current marker/lifecycle/capability badges; filter cycling; forced refresh; error/Esc cancellation; selected-id handoff.
- Existing `/model` command test now proves the typed request carries the live route. Full prior TUI/agent/composition suites remain green.

Primary code:

- `crates/heycode-agent/src/{ui,commands}.rs`
- `crates/heycode-agent/tests/law.rs`
- `crates/heycode-tui/src/{model_picker,command_palette,app,render,lib}.rs`
- `crates/heycode-tui/tests/model_picker.rs`
- `crates/heycode-cli/src/main.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 458 passed, 0 failed across 82 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U11` enforces CMD01 timing during active turns: immediate executes now, queued announces and runs after settlement, interrupting requires an explicit confirmation path, and model-scheduling stays durable/narrated.

## 2026-08-25 — U11: deterministic active-turn command scheduling

Tracker: `U11` complete.

Outcome:

- Added a total `route_command` policy over every CMD01 timing: idle work executes now; active-turn Immediate executes now, Queued/ModelScheduling enqueue, and Interrupting requests confirmation.
- `AppState` now tracks active-turn lifecycle independently from spinner presentation. Exact queued command text stays private in FIFO order; transcript narration uses only the validated descriptor synopsis and never echoes arguments.
- Queued work promotes only after both the turn and command task handles settle. Availability and timing are rechecked at dispatch, so stale prerequisites or programmatic submissions cannot bypass the policy.
- Slash commands execute in a separately joined async task instead of being awaited inline. Immediate commands therefore leave terminal, approval and agent event handling responsive; model-scheduling commands can call logged `Agent::send` without nesting a frozen loop.
- Interrupting commands render a centered cancel-default confirmation. Esc restores the byte-equivalent composer text, while explicit Interrupt confirms the one current cancel handle and queues the command until the turn settles.
- The native Agent still owns a process-lifetime cancellation token. U11 does not claim safe post-abort reuse; A04 remains the required reusable per-turn cancellation boundary.

TDD evidence:

- Red: the U11 integration suite could not call queue promotion, and the runner still awaited every command inline with no command task or dispatch-time timing/availability enforcement.
- Green: the timing matrix, immediate admission, FIFO queued/model-scheduling promotion, argument-free narration, unavailable refusal, cancel-default modal, exact composer restore and post-settlement interrupt execution pass.
- All prior TUI palette, model picker, welcome, onboarding, secret and approval frame suites remain green.

Primary code:

- `crates/heycode-tui/src/{command_scheduling,app,render,lib}.rs`
- `crates/heycode-tui/tests/command_scheduling.rs`
- `AGENTS.md`, `FEATURES.md`, `README.md`
- `docs/{STATUS,GOTCHAS}.md`
- `docs/engineering/{ARCHITECTURE,UI,TASKS,IMPLEMENTATION_LOG}.md`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 461 passed, 0 failed across 83 non-empty suites on macOS.
- Isolated real-binary `--fake doctor --json` — pass.
- Isolated real-binary `--fake run "smoke"` — pass.

Operational note: the first doctor attempt created but failed to pass a temporary `HEYCODE_HOME`, so the real user config followed its normal schema-1→4 migration and received a `.v1.bak`. Redacted normalized hashes prove only the schema line changed; no provider request or credential resolution ran. The corrected commands used the explicit absolute temporary home and passed. See GOTCHAS #78.

Next boundary:

- `CMD05` implements `/init` as a plugin-owned preview/apply workflow with project-law preservation and exact command inventory attribution.

## 2026-08-25 — CMD05: preview-tokened managed `/init`

Tracker: `CMD05` complete.

Outcome:

- Added crate/plugin `heycode-init`; the generic agent and binary own no AGENTS.md generation policy. The production profile mounts `init` immediately after `commands`, and exact inventory attributes command `init` to that plugin.
- `/init` and `/init preview` are strictly read-only. They inspect fixed root manifest names, build one versioned managed guidance section, render a 48 KiB-capped create/append/refresh diff and issue a 128-bit SHA-256-derived token over observed and proposed state.
- `/init apply <token>` accepts a closed lowercase-hex token, re-derives workspace/file state twice, rejects stale previews, and commits atomically. Existing bytes outside the exact marker range and existing file mode remain byte-identical; new files are `0644`.
- Targets are bounded to 1 MiB UTF-8 regular files. Symlinks, non-files, oversized content and missing/repeated/reversed markers fail before mutation. Preview text contains no absolute workspace path or existing project-law content.
- The command is Queued under U11 and emits generic human `UiEvent::Info`. A production-composition integration test invokes preview/apply and proves the session log remains empty.
- Added effect-owned late command registration. `init`, `skills`, `skill` and `plan` now dispose exact token-matched rows on rollback/shutdown; registry-lifetime `register_shared` remains only for explicit non-plugin callers.
- Generated built-in-profile migration evidence now includes `init`; explicit custom profiles remain byte-preserved and opt in deliberately.

TDD evidence:

- Red: no `heycode-init` crate/API/plugin existed; the new workflow suite failed on every unresolved contract.
- Green: create, append, managed-only refresh, unchanged idempotence, mode preservation, stale concurrent edit, unsafe target, malformed marker, command grammar, plugin disposal and production command/session isolation pass.
- Exact production command catalog, plugin list/source attribution, `/plugins verbose`, real-composition harness and config migration suites include the new capability.

Primary code:

- `crates/heycode-init/src/{error,model,service,plugin,lib}.rs`
- `crates/heycode-init/tests/init_workflow.rs`
- `crates/heycode-agent/src/{commands,plan}.rs`
- `crates/heycode-skills/src/lib.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{init_command,composition,plugin_inventory,real_composition_harness}.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 467 passed, 0 failed across 85 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `R01` defines the effect-owned `AgentRuntime` registry/session contract that keeps delegated Codex/Claude/OpenCode loops distinct from inference adapters; A01 then mounts the native loop behind that surface before U08/CMD02.

## 2026-08-25 — R01: native/delegated AgentRuntime kernel

Tracker: `R01` complete; actual native registration remains `A01`.

Outcome:

- Added lower-layer crate/plugin `heycode-runtime`; it depends only on core/LLM contracts so `heycode-agent` can implement the native adapter in A01 without a cycle.
- Added exact inventory namespace `agent_runtime`, deliberately distinct from `inference_provider`, and service `runtimes`. The production base plugin publishes an empty registry; it does not invent a native row before the real adapter exists.
- `AgentRuntimeDescriptor` validates lowercase kebab ids, bounded display names, explicit `Native|Delegated` loop ownership and tri-state models/resume/fork/steer/follow-up/permission/question/compaction evidence.
- Credential-free account state, absolute-workspace start/resume/fork requests, bounded human input, and external session/turn/request newtypes validate at their boundaries.
- `AgentRuntime` covers cancellable account/model discovery and start/resume/fork. `RuntimeSession` covers event subscription, send/steer/follow-up, cancel without close, correlated permission/question responses, native compaction and quiescent idempotent close; every async operation receives one caller token.
- Added provider-neutral sequenced event types for session/turn, commentary/reasoning/final, tools, permission/question, usage, settlement and notices. R02—not individual UIs—will validate sequence/bounds/settlement and project them.
- Runtime errors expose a stable class plus at most 512 trimmed control-free bytes. Fixed cancelled/closed/unsupported/internal constructors cannot leak provider bodies or subscription credentials.
- Registry ids/descriptors are deterministic and sorted; duplicates fail before publication; token-matched context disposal removes only the exact implementation.
- Production composition/default migration/service-key/inventory tests mount `runtimes`; it remains observably empty until A01.

TDD evidence:

- Red: `heycode-runtime` had no contract types, traits, registry, service or inventory namespace.
- Green: metadata rejection, native/delegated distinction, account/model discovery, start/resume/fork, every session control, normalized events, cancellation class, closed-session refusal, registry sorting/duplicates/effects and descriptor-family inventory pass.
- Real composition, exact service/plugin descriptors, `/plugins verbose` and generated-profile migration include the base runtime kernel.

Primary code:

- `crates/heycode-runtime/src/{error,id,model,traits,registry,plugin,lib}.rs`
- `crates/heycode-runtime/tests/runtime_contract.rs`
- `crates/heycode-core/src/inventory.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,real_composition_harness}.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 472 passed, 0 failed across 86 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `A01` adapts and registers the existing native `Agent` behind `AgentRuntime`, preserving current durable turn/cancellation/session behavior through the new registry before U08 consumes it.

## 2026-08-25 — A01: existing native loop behind AgentRuntime

Tracker: `A01` complete; reusable cancellation remains `A04`.

Outcome:

- Added separate provider plugin `runtime-native` after `agent`; it injects `runtimes`, `agent` and `models`, declares exact `agent_runtime:native`, and registers the actual composed Agent as an effect. No CLI branch or registry special case constructs it.
- Native descriptor reports explicit support for model discovery/current-session resume/compaction and explicit Unsupported for fork/steer/follow-up/runtime permission/question answers. Account state is Unknown without credential inspection.
- Start/resume must match the composed heycode session id, absolute workspace and provider-native session id. Start rejects a non-empty durable log as Conflict; resume reuses the exact current session. Optional start model updates the live Agent route.
- Send serializes through one async gate and calls the unchanged `Agent::send`; canonical runtime turn id matches the durable turn. Caller/external cancellation waits for the existing Agent to append `TurnEnd::Aborted` before returning Cancelled. Compact shares the gate and active-operation counter.
- Close is idempotent: mark closed, request cancellation and wait for active operations to settle. Context shutdown cancels held native sessions before removing registry/listeners; later sends fail safely.
- Added a bounded 1024-event replay+broadcast hub. Committed Session events provide turn/chunk/final/tool/usage/settlement; UiEvent contributes only permission requests/notices, avoiding duplicated/pre-commit assistant/tool facts. Snapshot+subscribe holds one state lock; lag is a protocol error.
- Added `EventBus::on_effect`; exact erased listener cells are removed on rollback/shutdown. Native registration, UI listener and session listener now unwind LIFO.
- Production profile, generated-profile migration, exact descriptor/inventory and `/plugins verbose` include `runtime-native`; live registry and exact `agent_runtime` rows are cross-checked.

TDD evidence:

- Red: no `native_runtime_plugin` or AgentRuntime implementation existed.
- Green: a real FakeProvider turn is started through `runtimes.get("native")`, emits replayable normalized committed events, preserves the existing session log, resumes, refuses restart/fork, maps missing catalog safely and disappears on shutdown.
- Hanging-provider cancellation returns Cancelled only after a durable aborted turn; close is idempotent and refuses new sends. Existing direct native turn/compact/cancel suites remain green.
- Core event-listener regression proves a held EventBus stops delivery after its owning context shuts down.

Primary code:

- `crates/heycode-agent/src/{native_runtime,agent,lib}.rs`
- `crates/heycode-agent/tests/native_runtime.rs`
- `crates/heycode-runtime/src/{error,id}.rs`
- `crates/heycode-core/src/events.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,real_composition_harness}.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 475 passed, 0 failed across 87 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `A04` replaces the Agent's process-lifetime cancellation token with a reusable per-turn owner, then proves both direct Agent and `runtime-native` can complete a fresh turn after abort.

## 2026-08-25 — A04: reusable per-turn cancellation owner

Tracker: `A04` complete.

Outcome:

- Added `AgentCancellation`: one context-owned terminal shutdown token plus one optional active child token guarded by an opaque identity. `cancel()` only cancels the current child and is a no-op while idle; lease drop cannot clear a newer generation.
- Agent's async turn gate serializes direct sends. Every turn acquires a fresh lease before its first durable append and drops it on every return path. Agent plugin owns the sole terminal shutdown effect.
- Added public `send_cancellable`. A caller token races while waiting for the gate, so pre-admission cancel writes no user/turn events and makes no provider call; mid-stream cancel follows the existing partial assistant + step end + aborted turn transaction.
- Native runtime creates/stores a child of its caller token before Agent dispatch. Session cancel/close can therefore land before Agent's future is first polled, while waiting for Agent's gate, or during streaming. The identity-checked active-operation guard clears it after settlement.
- Native compact shares the exclusive gate/active lifecycle. Close cancels the current operation and waits for zero active work; context shutdown reaches Agent's terminal token.
- TUI stores the caller token before spawning a turn and keeps one reusable `Fn + Send + Sync` interrupt closure. Esc, Ctrl+C and interrupt confirmation borrow it instead of consuming it; the closure also reaches model-scheduling Agent sends through `AgentCancellation`.

TDD evidence:

- Red: direct and runtime-native second turns returned Aborted/Cancelled after the first cancel; the TUI's FnOnce handle disappeared after one Esc.
- Green: both direct Agent and runtime-native abort a hanging first response, durably settle it, then complete a second provider response. Idle/pre-admission cancellation writes zero events/requests and retry succeeds.
- Unit leases prove idle no-op, generation-safe fresh children, shutdown cancellation and rejection. TUI regression invokes the same handle across two settled turns.
- Existing approval modal precedence, U11 command scheduling and native runtime close/shutdown tests remain green.

Primary code:

- `crates/heycode-agent/src/{cancellation,agent,native_runtime,plugin,lib}.rs`
- `crates/heycode-agent/tests/{turn,native_runtime}.rs`
- `crates/heycode-tui/src/app.rs`
- `crates/heycode-tui/tests/frames.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 479 passed, 0 failed across 87 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `U08` builds the provider/runtime picker over live inference and `AgentRuntime` registries, keeping native inference routes and delegated loop ownership visibly distinct before CMD02 persists selection.

## 2026-08-25 — U08: combined live provider/runtime picker

Tracker: `U08` complete; persisted selection remains `CMD02`.

Outcome:

- `/provider` without args now emits typed `RoutePickerRequested { current_provider, current_runtime }`; explicit provider ids retain the compatibility live setter path.
- Added pure `route_picker` projection over complete `ProviderProfile` and `AgentRuntimeDescriptor` rows. Runtime rows and inference rows remain separate identities/classes; current rows sort first within their plane.
- The centered modal renders loud `INFERENCE API`, `NATIVE AGENT` and `DELEGATED AGENT` badges, exact ids, display names, provider defaults/runtime capabilities, dual current markers and visible unavailable prerequisites.
- Fuzzy ranking covers id/display/detail/class using the shared matcher. Tab cycles All, Inference and Agent runtime filters; arrows wrap, Backspace/paste/input are bounded and Esc closes.
- Registered inference rows select the provider-owned default model for the live native session and update welcome/status. Current native runtime selection is a truthful no-op. Delegated/non-current native rows have no selection and cannot mutate effective state until a primary-session bridge exists.
- Status now renders `runtime · provider/model`, preventing a Codex/Claude delegated loop from being confused with an API provider route.
- TUI's delayed runtime-registry dependency is now mandatory in both plugin injects and LoopDeps. Root config schema is 5; v4→v5 losslessly inserts `runtimes` immediately before existing `tui` in custom complete profiles, while earlier dependency migrations accumulate.
- Production CLI passes the exact composed runtime registry; no TUI-side provider/runtime table or optional fallback exists.

TDD evidence:

- Red: no route-picker module/state/render/event/LoopDeps path existed, and heycode-tui had no runtime dependency.
- Green: mixed registries preserve all three classes, current/availability truth, fuzzy class/detail search and filters. Frames render every badge/prerequisite/status plane; delegated Enter stays open, while inference Enter returns only the typed live selection.
- Agent command regression proves `/provider` carries both current ids. Schema-v5 v4 custom-profile migration, real composition/e2e and all existing TUI modal suites pass.

Primary code:

- `crates/heycode-agent/src/{ui,commands,native_runtime}.rs`
- `crates/heycode-agent/tests/law.rs`
- `crates/heycode-tui/src/{route_picker,app,render,plugin,lib}.rs`
- `crates/heycode-tui/tests/route_picker.rs`
- `crates/heycode-cli/src/main.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 482 passed, 0 failed across 88 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `CMD02` moves provider/model selection through the settings CAS writer, then implements connect/logout/provider/model/effort command workflows over U06/U07/U08 without silently activating delegated rows or discarding opaque session state.

## 2026-08-25 — CMD02: persisted routing and split connection commands

Tracker: `CMD02` complete; reasoning effort remains correctly capability-gated.

Outcome:

- Added crate `heycode-routing` and service `routing`. Core plugin `routing` owns Settings namespace `routing` plus `/provider`, `/model`, `/effort`; optional `routing-auth` owns `/connect`, `/logout` and alone injects credentials/authorization/onboarding.
- Removed legacy provider/model command objects from generic `commands`; all five descriptors, availability and exact inventory now come from their owning plugins. Default command catalog contains fourteen rows.
- Routing settings store runtime/provider/model together; effort must be null until an active adapter exposes/consumes exact levels. Schema captures composed provider ids and only the currently activatable native runtime, rejecting unknown routes/fields and partial explicit tuples.
- Startup config selection is the composition base. User `settings.toml` routing state overrides it and is restored in real composition. A committed-settings watcher applies valid external generations live.
- Provider selection resolves the registered provider-owned default. Non-default model selection requires current selectable catalog evidence. Both read expected revision, durably replace the complete user section, then publish the returned effective provider/model to Agent.
- U07/U08 picker selection now calls RoutingService rather than mutating Agent. Explicit `/provider <id>` and `/model <id>` use the same path; no-arg forms emit typed picker requests from authoritative persisted state.
- `/connect` without args reopens U06 at runtime-class selection; an explicit provider/flow target must uniquely match a contributed AuthorizationDescriptor and uses its masked validation/commit path. `/logout` maps provider-owned credential reference to one flow query and calls authoritative delete; read-only shadows fail.
- `/effort` remains discoverable with a precise unavailable reason. Persisting a value that no current request consumes is forbidden.
- Root config schema is 6. v5→v6 inserts routing and only its minimal settings/models/runtime-native prerequisites before TUI. `routing-auth` is not injected into intentional minimal profiles. The historical minimal real-binary e2e exposed and now prevents accidental full auth-stack expansion.

TDD evidence:

- Red: no routing crate/service/namespace existed; provider/model commands mutated live Agent only; connect/logout/effort commands were absent.
- Green: production selection persists before live apply, increments revision, emits typed pickers, applies external committed reloads and restores after a fresh real composition. Command source ownership and effort availability are exact.
- Routing-owner tests cover base/user resolution and rejection of unknown routes, explicit incomplete tuples, effort and unknown fields. Connect reopens inactive onboarding; logout without provider credential metadata fails safely.
- Schema-v6 migrations, minimal custom profile e2e, exact services/settings/commands/plugins, `/plugins verbose`, all TUI suites and real composition are green.

Primary code:

- `crates/heycode-routing/src/{error,model,service,commands,plugin,lib}.rs`
- `crates/heycode-routing/tests/routing_settings.rs`
- `crates/heycode-agent/src/{commands,plugin,ui,native_runtime}.rs`
- `crates/heycode-onboarding/src/lib.rs`
- `crates/heycode-tui/src/{app,plugin}.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-cli/tests/{routing_commands,composition,plugin_inventory,real_composition_harness,e2e}.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 489 passed, 0 failed across 90 non-empty suites on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — pass.
- Explicit temporary-home real-binary `--fake run "smoke"` — pass.

Next boundary:

- `CMD03` contributes effective `/status`, `/doctor`, `/permissions` and `/sandbox` commands/panels; U09 permission/sandbox selection is the dependency-critical first sub-boundary.

## 2026-08-25 — E02: exact subprocess and process-tree service

Tracker: `E02` complete; shell resolution and complete sandbox/process-consumer routing remain `E03`/`E04`.

Outcome:

- Added low-level crate `heycode-exec`, typed service `subprocess` and built-in provider plugin `subprocess-local`. The binary only registers/composes the plugin; consumers depend on `SubprocessService`/`SubprocessBackend`.
- `ProcessSpec` accepts only absolute executable and cwd paths, exact argv and a complete explicit environment. The local provider always clears inheritance before applying those values. NULs, invalid/duplicate environment names, excessive components/spec bytes, zero/>24-hour timeouts and zero/>64 MiB capture limits fail before spawn.
- Capture is fail-loud and bounded per stream. Stdout remains exact bytes; stderr is explicitly documented as decoded text. Nonzero exits and wall/inactivity timeouts are typed results. Spec/result debug output contains counts/limits only, and infrastructure errors retain a fixed code/message with no argv, environment value, captured body, path or raw OS error.
- Every spawn receives one processkit 3.3.4 private group. Managed handles are non-cloneable; wait/cancel/terminate/kill consume the handle, so terminal ownership cannot race. Explicit cancellation waits for confirmed quiescence. Drop and plugin-context shutdown cancel and hard-kill the contained tree.
- Caller cancellation forwards into the operation token through a handle-owned task. Async terminal paths abort and join that task; synchronous drop aborts it. The context owns the parent shutdown token as a disposer.
- Spawn-free capability reporting identifies Job Object, cgroup v2, POSIX process group, process reaper or an unknown future mechanism, and explicitly marks POSIX process groups as vulnerable to deliberate `setsid` escape rather than overstating containment.
- Production composition, dry inspection, built-in key registry, default profile, profile-migration fixture, exact descriptors and `/plugins verbose` now include the provider/service. Schema remains v6 because no persisted format or new dependency of an existing custom-profile plugin changed.
- Existing bash, MCP and command-credential process paths are deliberately not claimed as migrated. E03 defines the shell request/spec Consumer; E04 routes every process Consumer through the sandbox/service path.

TDD evidence:

- Red: the new contract suite initially failed to compile because no process types/service/plugin existed. Production composition then failed its exact service and plugin inventories after the local suite first went green.
- Green: a real helper binary proves inherited `PATH` is absent while an explicit value arrives; nonzero output is returned without body leakage; timeout is a result; output overflow and missing executable failures are body/path-free.
- Natural wait, caller-token cancellation, explicit cancel, graceful terminate, hard kill, handle drop and context shutdown all settle. Two tests spawn a real descendant that writes a delayed survival marker; hard kill and handle drop both prevent the marker.
- Boundary validation, plugin owner/effect teardown and post-shutdown new-work refusal are pinned.

Primary code:

- `crates/heycode-exec/src/{error,model,service,local,plugin,lib}.rs`
- `crates/heycode-exec/tests/subprocess_contract.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,real_composition_harness}.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 498 passed, 0 failed across 91 non-empty suites on macOS.
- Explicit absolute temporary-home real-binary `--fake doctor --json` — schema 1, healthy.
- Same isolated real-binary `--fake run "smoke"` — `[done:stop]`.

Next boundary:

- `E03` adds the one explicit `resolve(ShellRequest) -> ShellSpec` defaulting boundary and a shell Consumer over `subprocess`; `E04` then applies sandbox policy to the resolved process spec and migrates every direct process path.

## 2026-08-25 — E03: resolved shell boundary and real bash Consumer

Tracker: `E03` complete; common sandbox routing and remaining direct process Consumers remain `E04`.

Outcome:

- Added service `shell` and split built-in provider `shell-local` in `heycode-exec`. It injects `subprocess`; production `tools` now injects `shell`, making the dependency visible to dry composition and profiles.
- `ShellRequest` represents only intent: exact command plus optional cwd, timeout and output-cap overrides. Blank/NUL/>1 MiB commands, relative cwd, zero/>24-hour timeout and invalid caps fail at construction.
- `ShellBackend::resolve` is the only default owner. It materializes a validated `ShellSpec` containing platform shell id + absolute executable argv, absolute cwd, one credential-name-scrubbed environment snapshot, nonzero timeout and bounded-tail capture policy. `execute` accepts only `ShellSpec` and delegates its unchanged `ProcessSpec` to `subprocess`.
- `ShellSpec::with_launch_argv` is the temporary E03 policy bridge: existing ToolCtx sandbox confinement transforms the already-resolved argv, while cwd/environment/timeout/capture are revalidated and byte-preserved. E04 moves this policy below every process Consumer.
- The model-facing `bash` implementation contains no `tokio::process`, child, pipe, timeout or environment code. It parses optional timeout strictly, resolves through `shell`, applies the current sandbox transform, executes with the ToolCtx cancellation token, and formats typed exit/signal/timeout results.
- Output uses processkit bounded-tail retention. A prefix drop is explicit in `ProcessOutput::truncated`; bash retains its trailing 64 KiB and adds a visible truncation marker. Infrastructure/overflow/cancellation failures remain fixed and body-free.
- `ToolCtx` now owns an operation cancellation token. Agent races turn and caller cancellation during tool work, cancels the tool token, and awaits settlement rather than dropping the tool future. A live regression starts a bash background descendant, cancels the turn, and proves durable abort returns only after the descendant cannot write its survival marker.
- ACP `session/new { cwd: "." }` exposed that protocol cwd was entering composition relative. The boundary now joins relative client cwd to the server's absolute process cwd; the absolute `ProcessSpec` contract was not weakened.
- Config schema is 7. Migration inserts `subprocess-local`, then `shell-local`, immediately before an existing `tools` in every older custom profile. Generated-profile restoration picks up both automatically; optional unrelated plugins remain untouched.
- Added a combined `execution-local` plugin only for hand-built embedded/test compositions. The production factory continues using separately inspectable `subprocess-local` and `shell-local` rows.

TDD evidence:

- Red: the shell contract initially failed to compile because no shell types/service/plugin existed. After the service went green, production/custom composition exposed the new unsatisfied `tools → shell` dependency and the ACP relative-cwd protocol gap.
- Green: shell tests prove complete one-shot default materialization, safe environment names, exact execution, nonzero/timeout result semantics, pre-cancel error semantics, secret-free request/spec Debug, wrapper validation, plugin inject/ownership and post-shutdown refusal.
- Bash tests prove strict timeout parsing, cwd, stderr/nonzero, timeout, scrubbed environment and visible bounded-tail retention. The existing live Seatbelt test still proves inside-write/outside-deny through the resolved wrapper path.
- Schema-v7 tests prove both new dependency rows and accumulated v2–v6 migrations. Real binary ACP, default/custom composition, exact inventory, setup/profile migrations and fake write/edit/bash e2e all pass.

Primary code:

- `crates/heycode-exec/src/{shell,model,local,lib}.rs`
- `crates/heycode-exec/tests/shell_contract.rs`
- `crates/heycode-tools/src/{plugin,tool,config,builtins/bash}.rs`
- `crates/heycode-agent/src/agent.rs`
- `crates/heycode-agent/tests/sandbox.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 505 passed, 0 failed across 92 non-empty suites on macOS.
- Explicit absolute temporary-home real-binary `--fake doctor --json` — schema 1, healthy.
- Same isolated real-binary `--fake run "smoke"` — `[done:stop]`.

Next boundary:

- `E04` moves sandbox policy/backend application into the common execution path and migrates MCP plus command credentials so no product process launch bypasses the service/policy boundary.

## 2026-08-25 — E04: mandatory sandbox routing for every process Consumer

Tracker: `E04` complete; richer guarantee/capability reporting remains `E10`, with OS runtime CI in E11–E13.

Outcome:

- Moved `Sandbox`, `SandboxPolicy`, `SandboxMode`, `SandboxError`, `SandboxService` and service key ownership down from `heycode-tools` into `heycode-exec`. `heycode-sandbox` now owns only Seatbelt/bwrap/Landlock Provider implementations and plugin publication.
- `sandbox` is always a composed service and default plugin before `subprocess-local`. Effective `Off` truthfully has no backend but remains the mandatory path; read-only/workspace require one backend. `subprocess-local` injects it and applies the wrapper after all defaults but immediately before capture, ordinary spawn or interactive spawn.
- `ProcessSpec::with_launch_argv` revalidates wrapper program/argv while preserving cwd, complete environment, timeout, output limit/policy and interactive mode. Sandbox/backend bodies never enter `ProcessError`.
- Removed sandbox state from `AgentOptions`, `Agent`, `SubagentRunner` and `ToolCtx`; bash contains no policy branch. The execution backend is now the only production decision point.
- Added provider-neutral `InteractiveProcess`, `ProcessInput` and `ProcessLines` contracts. The local Provider uses processkit open stdin + streamed stdout while retaining the same private group and cancellation owner.
- Migrated MCP off `tokio::process`. It resolves an exact executable through `SubprocessService`, merges a scrubbed parent environment with explicit server overrides, uses bounded interactive stdio, and owns a driver stop token + plain driver thread + runtime. Shutdown cancels the driver, drops the private process group and joins the driver; final Tokio runtime drop happens on a plain thread, avoiding async-context runtime-drop panics.
- Migrated configured command credentials off direct child/process APIs. The synchronous credential trait still bridges through a named worker/current-thread runtime, but executable resolution, explicit env, capture, timeout, sandbox and whole-tree cleanup all delegate to the composed service. The empty built-in provider does not manufacture a dependency it cannot exercise; a configured provider declares it.
- Added `process_spawn_law`: production Rust source outside `heycode-exec` cannot introduce `std/tokio::process` spawn/output. Landlock's in-place `CommandExt::exec` is the one named backend exception; test probes are out of production scope.
- Config schema is 8. Sandbox is inserted before subprocess; legacy MCP profiles receive missing shell/subprocess/sandbox dependencies. The service key remains one row but is no longer conditional.

TDD evidence:

- Red: the initial E04 test could not import a sandbox service from `heycode-exec`; after centralization, existing local subprocess tests failed their newly explicit inject until off-policy Providers were composed.
- Green: a recording backend proves ordinary output and interactive stdio each cross the exact effective policy once. Off/read-only/workspace shape validation and post-shutdown service behavior remain typed.
- The existing live Seatbelt agent test still allows workspace writes and denies outside writes without Agent/ToolCtx sandbox state.
- MCP plugin composition records one sandbox call, protocol behavior remains green, and a real MCP parent spawns a delayed child that cannot write after context shutdown.
- A command-credential recording backend proves policy traversal; a timed helper's delayed descendant cannot survive the returned timeout.
- Source-law, schema-v8 v7-MCP migration, exact plugin/service/inventory, default/off and active sandbox composition all pass.

Primary code:

- `crates/heycode-exec/src/{sandbox,service,local,model,shell,plugin,lib}.rs`
- `crates/heycode-exec/tests/sandbox_routing.rs`
- `crates/heycode-sandbox/src/{lib,landlock}.rs`
- `crates/heycode-mcp/src/lib.rs`
- `crates/heycode-credentials-command/src/{provider,plugin}.rs`
- `crates/heycode-agent/src/{agent,plugin,subagent}.rs`
- `crates/heycode-cli/src/lib.rs`
- `crates/heycode-cli/tests/{composition,plugin_inventory,process_spawn_law,real_composition_harness}.rs`
- `crates/heycode-config/src/migration.rs`
- `crates/heycode-config/tests/migrations.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — 513 passed, 0 failed across 94 non-empty suites on macOS.
- Explicit absolute temporary-home real-binary `--fake doctor --json` — schema 1, healthy.
- Same isolated real-binary `--fake run "smoke"` — `[done:stop]`.

Next boundary:

- `E10` publishes truthful effective sandbox/backend guarantees, making unsupported choices unselectable; U09 and CMD03 then consume that same report for permission/sandbox UI and commands.

## 2026-08-25 — E10/E11/E12/E13: truthful sandbox choices and native evidence lanes

Tracker: `E10` and `E12` complete. `E11` remains active until hosted native Landlock CI passes. `E13` remains active because native CI has not run and Windows filesystem confinement is not implemented.

Outcome:

- `SandboxService::capability_report()` publishes effective mode, active vs merely available backend and stable Full Access / Read Only / Workspace Write rows. Read-only/workspace selectability requires `Supported`; absent/Unknown/Unsupported remains visible and unselectable. Network scope is independent and current backends report host networking. Workspace Write names backend temp roots without claiming isolation: Seatbelt/Landlock use host temp, while bwrap uses private tmpfs.
- Full Access retains a candidate backend for restrictive choices but applies zero argv transforms. Restrictive composition fails if the selected guarantee is not evidenced.
- Corrected Landlock v1 UAPI bits, hard-required live ABI-v1 restriction, conditionally added ABI-v2…v8 filesystem rights, verified no-new-privs/restriction status and omitted nonexistent temp grants. ABI-v9 Unix-socket mediation is deliberately excluded to preserve the host-network claim.
- Added strict Linux Landlock/bwrap matrices and SHA-pinned Ubuntu workflow. The workflow also builds/lints `heycode-cli` and exercises the production `heycode __landlock <rules> -- <argv>` boundary before the matrix. Real Linux Docker proves bwrap and Landlock-unavailable fallback; Docker Desktop cannot prove Landlock, so the hosted lane stays open.
- Added native macOS Seatbelt path/write/symlink/temp/device/TCP/Unix matrix and SHA-pinned `macos-15` workflow; the current Mac passes both tests.
- Added Windows Job Object cancellation/kill/drop/descendant tests, truthful unsupported restrictive choices and a SHA-pinned `windows-2022` workflow. Cross-target clippy/actionlint pass. Job Objects are process containment only; the current argv transform cannot create AppContainer/restricted-token children.

TDD/review evidence:

- E11 design immediately exposed the incorrect Landlock rights and unconditional `/private/tmp` assumption; denial helpers use unique exits/markers so launcher failure cannot false-pass.
- E13 found a Windows-only E01 `unused_mut`; cfg-specific binding fixed it before admission.
- Local sandbox package tests now cover 13 tests; native workflows remain the stronger platform evidence.

Primary code:

- `crates/heycode-exec/src/sandbox.rs`
- `crates/heycode-exec/tests/{sandbox_capabilities,sandbox_routing}.rs`
- `crates/heycode-sandbox/src/{lib,landlock}.rs`
- `crates/heycode-sandbox/tests/{linux_runtime,seatbelt_runtime,windows_runtime}.rs`
- `.github/workflows/{linux-sandbox,macos-seatbelt,windows-sandbox}.yml`

## 2026-08-25 — U09/CMD03: effective permission picker and status commands

Tracker: `U09` and `CMD03` complete.

Outcome:

- TUI projects the authoritative E10 report into three rows with exact file read/write/network guarantees, active/available backend, current state and visible unavailable reason. Security modals preempt it; keyboard selection never chooses unsupported rows.
- A supported different selection is intent only and produces the exact restart prerequisite. No Settings mutation/status fiction exists. Selecting the current mode is a clean no-op.
- New plugin crate `heycode-status` contributes immediate `/status`, `/doctor`, `/permissions` and `/sandbox`. It reads live Agent/Doctor/Sandbox state, emits only human-plane events, reports host network as not isolated and disposes commands/cancellation with Context.
- Default composition and exact command/inventory assertions include plugin `status`; intentional exact custom profiles are unchanged.

Evidence: 7 permission-picker frame/keyboard tests and 4 status-command tests pass; production composition attributes all four commands to `status`.

Primary code:

- `crates/heycode-tui/src/{app,render,permission_picker}.rs`
- `crates/heycode-tui/tests/permission_picker.rs`
- `crates/heycode-status/`
- `crates/heycode-agent/src/ui.rs`
- `crates/heycode-cli/src/lib.rs`

## 2026-08-25 — E01: replaceable filesystem Service Definition and Consumers

Tracker: `E01` complete; canonical root/symlink/TOCTOU remains `E05`, retained spill remains `E06`.

Outcome:

- `heycode-exec` owns `FileSystemBackend`, `FileSystemService`, explicit request/spec/result/error types, Provider observation records and plugin `filesystem-local` under service `filesystem`.
- Local Provider supplies bounded text reads/binary probe, metadata/directories, read-before-overwrite, fresh-read-before-edit, atomic mode-preserving write/edit and sorted/capped glob/grep.
- `read`, `write`, `edit`, `glob` and `grep` are Consumers; `tools` injects `filesystem`. A recording Provider proves every operation crosses the seam.
- Provider results validate ordering/count/size/portable relative paths. Wildcard matching is iterative at the 64 KiB pattern boundary. Multiline and line-boundary edits now publish exact prefixed diffs; zero-match `replace_all` fails before mutation.
- Schema v9 inserts `filesystem-local` before `tools` in every older custom complete profile. Default factory/service/inventory and manual test worlds use the same dependency.

TDD evidence:

- Initial contract test failed because E01 types did not exist.
- Root review added regressions for a committed multiline edit with a false error/wrong diff, zero-match replacement, trailing-newline replacement, traversal/control-bearing Provider output paths and recursive wildcard exhaustion.
- `heycode-exec` passes 30 tests; `heycode-tools` passes 72.

Primary code:

- `crates/heycode-exec/src/filesystem/`
- `crates/heycode-exec/tests/filesystem_contract.rs`
- `crates/heycode-tools/src/builtins/{read,write,edit,glob,grep}.rs`
- `crates/heycode-tools/tests/filesystem_provider.rs`
- `crates/heycode-config/src/migration.rs`

## 2026-08-25 — R02: delegated runtime event normalization

Tracker: `R02` complete.

Outcome:

- Added single-use `RuntimeEventNormalizer` and normalized live/replay stream wrappers. Sequence starts at zero with one session-ready; turn/tool/request identities, open calls, final/settlement order and bounded correlation memory fail closed.
- Text, structured JSON, ids/choices/notices are size/shape checked; terminal controls in nested JSON values and keys are rejected. Errors and `NormalizedRuntimeEvent` Debug expose only stable classes/phase.
- Live EOF is a protocol failure. Finite replay EOF succeeds only after session-ready and between turns. Multiple Usage phases remain valid because a runtime turn can contain multiple inner model steps.

Evidence: 10 normalization tests plus the 5 R01 runtime tests pass; malformed, EOF, redaction, correlation-cap and complete-phase replay contracts are pinned.

Primary code: `crates/heycode-runtime/src/{model,normalization}.rs`, `crates/heycode-runtime/tests/runtime_event_normalization.rs`.

## 2026-08-25 — C06: durable inbox splice projection

Tracker: `C06` complete; A03 consumes it for live wake/steer behavior.

Outcome:

- Added v2-only `agent/inbox/splice` with typed follow-up/steer/inject message, claim, cancel and replacement forms.
- Append validates normalized target/text/id/shape and one global insert-once identity ledger before durable write; publish follows commit. Resume rejects malformed/tampered/reused history.
- Pending next-turn/next-step and settled ledgers replay exactly. Compaction never shadows operational inbox state. Inbox text stays outside model projection until admitted through `user/message`.
- All five session-kind touchpoints, raw-v1 rejection and explicit projection ignore arms are tested.

Evidence: `heycode-session` passes 53 tests. Neutral tool-result projection was also tightened to retain durable `is_error` for provider adapters.

Primary code: `crates/heycode-session/src/{event,inbox,projection,request_projection,session}.rs` and inbox/provider-state/request/version tests.

## 2026-08-25 — P05: Anthropic Messages protocol adapter

Tracker: `P05` complete; PAN01 owns auth/catalog/profile activation and N02 owns automatic Pause continuation.

Outcome:

- Added reusable `AnthropicMessagesAdapter` with x-api-key/bearer routes, optional API version/extra headers, exact disabled/adaptive/manual thinking + wire effort and multiple exact server-tool definitions per logical Web feature.
- Requests replay complete ordered assistant blocks and continuation containers. Parallel client results coalesce into one immediate User content array; durable failure emits `is_error:true` without parsing text.
- Stream parser preserves thinking/signatures, redacted thinking, citations, client/server/programmatic tools, unknown complete blocks, one compaction delta and continuation containers. `pause_turn` is distinct `FinishReason::Pause`.
- Response model/lifecycle/index/tool advertisement, thinking signature order, content-after-message-delta, cache-component monotonicity/overflow, both null start-terminal fields, refusal/unknown stop and premature EOF fail without state/Finish.
- Prompt usage sums normal + cache-creation + cache-read input. Provider-controlled discriminator/error strings never enter diagnostics. Raw SSE fragmentation is invariant.

TDD/review evidence:

- Initial red builds lacked `AnthropicMessage` and `Pause`. Review then produced exact failures for separate parallel result messages, undercounted cache tokens, wrong response model, malformed sequencing/compaction and network-data error echo.
- `heycode-core` passes 30 tests; `heycode-llm` passes 110, including 15 Anthropic tests.

Primary code: `crates/heycode-llm/src/{anthropic,vocab,inference}.rs`, `crates/heycode-llm/tests/anthropic_protocol.rs`, `crates/heycode-core/src/vocab.rs`, session/agent mapping.

## 2026-08-25 — MCP01: inspectable MCP definition/generation registry

Tracker: `MCP01` complete; MCP02 moves the existing stdio bridge beneath it.

Outcome:

- Plugin `mcp-registry` publishes service `mcp`. Definition Providers register immutable exact stdio/Streamable HTTP definitions as effects; private literal-bearing values have no Debug/serde.
- Schema-v1 snapshots expose deterministic scopes/transports/value-source kinds, credential references, policies, state and successful generation metadata without literal bytes.
- Connection publishers are opaque-token guarded. Only a complete validated candidate advances the per-server generation and registry snapshot atomically; failures retain/remove last-good explicitly; stale owners and shutdown cannot publish.
- State agrees with definitions: successful credential-backed generations prove Connected; no-auth definitions cannot claim auth failure; reconnect attempts use the configured enabled budget. URL authorities/ports, tool-policy allowlists and result metadata fail closed.
- No fake Settings mutation was added; MCP10 owns legitimate persistence/management.

Evidence: 5 registry tests plus 5 legacy stdio tests pass, including secret canaries and child-tree shutdown.

Primary code: `crates/heycode-mcp/src/registry/`, `crates/heycode-mcp/tests/registry.rs`, CLI factory/service inventory.

## 2026-08-25 — PL01: plugin manifest v1 boundary

Tracker: `PL01` complete; PL02/PL03/PL05 own install, activation and signature verification.

Outcome:

- New pure crate `heycode-extensions` parses strict TOML schema v1 with API/platform compatibility, marketplace-namespaced ids, contribution paths/exposure, permissions/default enablement, credential references only, source/update metadata, dependencies/conflicts and SemVer bounds.
- Portable paths reject traversal, Windows drive/device/invalid/trailing forms (including COM/LPT superscript-digit aliases) and case-insensitive contribution collisions. Public names are deterministic unless an exact registry-authorized override is declared.
- Source authorities reject credentials/percent obfuscation/invalid ports; SHA-256 and canonical base64 64-byte Ed25519 metadata are exact. Single/batch collision checks are atomic and failures do not echo document bytes.
- This crate validates metadata only; no package files, network, install or activation side effects exist.

Evidence: 16 focused tests pass. Root review's first red cases proved drive paths, wrong-length Ed25519 values and invalid ports had been accepted before hardening.

Primary code: `crates/heycode-extensions/`.

## 2026-08-25 — parallel-wave integration gate

Tracker rollup after admission: **77 complete · 3 active · 212 not started · 0 blocked**.

Central integration:

- Default composition adds `filesystem-local`, `mcp-registry` and `status`; exact plugin/service/command/contribution inventories and real-composition harness are updated.
- Workspace now contains 33 crates and schema 9. `BUILTIN_SERVICE_KEYS` contains 30 exact rows including the pre-tool seam.
- Shared continuity documents, provider/UI/MCP/agent/architecture references and GOTCHAS #96–#103 describe implemented boundaries and explicit deferrals.

Verification:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace` — **607 passed, 0 failed across 109 non-empty suites** on macOS.
- Explicit temporary-home real-binary `--fake doctor --json` — schema 1 report, healthy, 4 passed/0 warning/0 failed/0 skipped.
- Same isolated real-binary `--fake run "smoke"` — `[done:stop]`.

Next dependency-ready work:

- Accept E11/E13 only after their missing native evidence/backend exists; do not weaken the criteria.
- Continue the critical product path through U10 first-run orchestration and provider/runtime activation dependencies; MCP02 and PL02 can proceed from the newly completed registries/contracts in separate file lanes.

## 2026-08-25 — second parallel wave dispatched

Current tracker: **77 complete · 12 active · 203 not started**.

Non-overlapping implementation lanes:

- U01+K12 — one coupled workspace-trust Service/guard boundary; resolve the tracker dependency cycle deliberately.
- P08 — provider error taxonomy/retry/cancellation with explicit adapter token flow.
- R03 / R07 — isolated Codex app-server and Claude CLI process/wire crates using read-only local metadata probes, never token scraping.
- C07 — session query/list/resume/fork lineage over JSONL truth.
- E05 — capability-directory canonical root/symlink/TOCTOU enforcement. Root approved Bytecode Alliance `cap-std 4.0.3` after source/version/advisory review; adversarial tests must prove rather than assume its behavior.
- MCP02 — existing stdio bridge beneath the MCP01 registry. Once `mcp` declared that real inject, root added schema-v10 migration and a red→green v9 custom-profile test inserting `mcp-registry` immediately before `mcp`.
- PL02 — content-addressed atomic install cache without activation/network/signature-verification scope creep.

Root retains Cargo/CLI/config/default-profile/shared-doc integration and final workspace gates. E11/E13/QSEC01 remain active evidence/approval lanes, not implementation-complete claims.

## 2026-08-25 — second-wave production gate

Tracker after admission: **86 complete · 3 active · 203 not started · 0 blocked**. The only active rows are E11, E13 and QSEC01 evidence/approval. Pending work is now classified as 53 actionable implementation, 15 actionable verification/docs, 126 dependency-blocked and 9 post-beta/optional. Per user direction, all future work is root-only; no further subagents are used.

Implemented and integrated:

- **U01/K12** — canonical pre-config workspace trust service; absolute-home and trust/cwd binding; restricted/trusted project gates; highest-priority live-bound TUI modal; session/persistent CAS actions; typed recompose/exit; ephemeral pre-trust sessions. Unix store operations are descriptor-relative, owner/mode/nlink checked and cross-process locked. Windows file persistence stays `UnsupportedSecurity` after proving `cap-std` directory creation falls back to an ambient path.
- **P08** — body-free provider error taxonomy, status-authoritative classification, semantic Retry-After/advice, bounded/deadlined error bodies, URL-free reqwest diagnostics, replay-safety policy and terminal/cancellable retries. DeepSeek now advertises strict production dispatch: auto-refresh exact catalog evidence, route-project durable inputs, resolve, append header/context, independently verify and consume the call. Provider state buffers until terminal Finish. OpenRouter remains compatibility-only until POR01/POR02.
- **R03** — new `heycode-runtime-codex`: exact 0.146.0, sanitized PATH, bound native Node interpreter/script identities, raw strict JSONL, out-of-order correlation, sticky terminal settlement, 8 MiB retained budget, caller-connected spawn cancellation and truthful containment. The real installed handshake passed after changing safe PATH normalization to drop absent absolute entries rather than rejecting common host PATHs.
- **R07** — new `heycode-runtime-claude`: reviewed 2.x version interval, one executable identity across version/auth/query, credential-blind status, explicit credential-free environment, strict known event phases and double no-persistence backstop. The installed 2.1.243 live gate passed.
- **C07** — constructor-owned `session/created`, safe provenance, effect-owned bounded `session-query`, deterministic filters/pagination/latest/resume/fork and raw-line-hash shared-prefix lineage. Native subagent fork no longer copies history and supports an exact zero-event prefix.
- **E05** — capability-rooted canonical filesystem policy, read/write grants, stable observation identities, read/write freshness, root/parent/target/absence rechecks and atomic no-clobber/mode-preserving mutation. Raw subprocess IO was added as a provider-neutral prerequisite for R03.
- **MCP02** — stdio definitions/connections publish beneath MCP registry; candidate tool rows use owned token handles, roll back atomically and disappear after transport-first shutdown even from held registries.
- **PL02** — Unix owner-only content-addressed cache with twice-validated descriptor-relative source traversal, portable/case-safe identity, immutable no-clobber refs, OS/process/stage leases, bounded capability cleanup and prior-version retention. A full-gate cleanup race exposed vanished lock metadata; cleanup now treats concurrent NotFound as benign and passed ten repeated stress runs. Non-Unix install remains fail-closed.

Central integration:

- Workspace is **36 crates**, current config is **schema 11**, and the exact service/seam registry is **32 keys**. Schema 11 inserts the newly explicit `models` dependency before historical custom `agent` profiles; optional delegated/session-query rows are not injected into intentional profiles.
- Default composition registers `trust`, `session-query-jsonl`, `runtime-claude` and `runtime-codex`; runtime discovery is sorted `claude`, `codex`, `native` without requiring optional executables at boot.
- The first complete test run exposed four older hand-built worlds/profiles missing `models` and one cache cleanup race. All received direct regression fixes before the accepted gate.

Accepted verification:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace --no-fail-fast` — **810 passed, 0 failed across 127 non-empty suites** on macOS.
- Isolated absolute temporary-home real-binary doctor — healthy, **4 passed / 0 warnings / 0 failed / 0 skipped**.
- Same isolated real binary `--restricted-workspace --fake run` — `[done:stop]`.
- Safe live installed Codex 0.146.0 and Claude Code 2.1.243 probes — pass.

Truthful deferrals:

- E11 still needs hosted native Landlock evidence; E13 needs a native Windows filesystem-confinement backend and protected handle-relative owner-security for durable trust/plugin cache.
- QSEC01 threat model is persisted, but root `SECURITY.md` still needs the security-policy workflow's explicit owner approval.
- Codex/Claude session bridges, OpenCode, OpenRouter strict catalog/profile activation, full MCP, plugin activation/signature verification and the other tracker rows remain open.

## 2026-08-25 — U10 first-run authorization and exact recomposition

Tracker: `U10` complete. Rollup after admission: **87 complete · 3 active · 202 not started · 0 blocked**.

Outcome:

- `OnboardingOutcome::ReadyToRecompose` and `TuiRunOutcome::RecomposeConnection` make Connected/Continue a typed product-shell transition. The disconnected composer is never exposed after a receipt.
- The CLI first shuts down the old Context, then drops the old Tokio runtime and invokes startup with the exact original argument vector. Explicit trust/profile/config/patch/provider/model/resume choices are preserved.
- API/router authorization choices remain descriptor-derived but are restricted to the effective provider, preventing an OpenRouter grant while DeepSeek remains selected. The selected provider's flow retains any custom configured credential reference.
- Fixed startup presence detection to inspect `llm.api_key_env` when supplied and fall back to the provider-owned default only when absent. Presence, validation, flow query and real LLM composition now agree on reference identity.
- Authorization already owned provider validation, credential write, validation-cache record and authoritative safe readback. Only the completed task publishes the Connected page. Failure stays recoverable on the method page; a class with no compatible flow renders one visible Back row.
- Every fallible event-loop path now resolves inside a nested async result boundary. Terminal close, explicit exit and errors cancel all operation tokens before authorization, admitted turn, command, doctor and model-wait handles are joined. Non-model command work is aborted+joined; model-scheduling work is cancelled through Agent so durable turn settlement survives.
- The first composer intentionally accepts the provider profile's registered default model and the live sandbox report's already-effective selectable choice. Forcing `/model` and `/permissions` before the first prompt would repeat valid committed defaults; both pickers remain immediately available.

TDD and composition evidence:

- Red: the error-path lifecycle test could not resolve `finish_authorization`, proving ordinary post-loop settlement did not structurally cover `?` exits.
- Red: the production orchestration test could not resolve `provider_key_present_at`; the existing function hard-coded provider-default environment names and could not prove a custom committed reference survived restart.
- Green: generic cancellation/join and authorization error-result tests observe task-owned settlement, not merely handle drop.
- Green: an isolated production-loader journey disables the process-global Keychain via a real User-scope profile, boots the normal disconnected/onboarding world, checks the selected DeepSeek flow owns the custom query, registers one deterministic validated test flow, commits through the real file provider, verifies authoritative readback, shuts down, proves startup presence at the configured reference and recomposes a real credential-backed DeepSeek world with inactive onboarding. No provider request or secret output occurs.
- Green: the recomposed Agent route equals the provider registry's default model, and the live sandbox effective row is selectable. Existing trust/secret/approval/palette/model/route/permission modal-priority suites remain green.

Primary code:

- `crates/heycode-onboarding/src/lib.rs`
- `crates/heycode-onboarding/tests/state_machine.rs`
- `crates/heycode-tui/src/app.rs`
- `crates/heycode-tui/tests/onboarding.rs`
- `crates/heycode-cli/src/{lib,main}.rs`
- `crates/heycode-cli/tests/first_run_orchestration.rs`

Verification at the feature boundary:

- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace --no-fail-fast` — **817 passed, 0 failed across 128 non-empty suites** on macOS.
- Explicit absolute temporary-home real-binary doctor — healthy, **4 passed / 0 warnings / 0 failed / 0 skipped**.
- Same isolated real binary `--restricted-workspace --fake run "smoke"` — `[done:stop]`.

Safety boundary:

- This test proves the production composition/credential/recomposition transaction without a real provider secret. Q16 still owns fresh-machine macOS/Linux/Windows real-turn certification; the known process-global dummy OpenRouter Keychain item remains untouched and local OpenRouter 401 evidence remains untrustworthy.

Next boundary:

- Continue root-only with `N01`, the logical provider-native tool registry/router. It must commit the resolved logical→implementation choice in `request/header` before any provider-specific native tool is activated.

## 2026-08-27 — POR01 provider-owned OpenRouter auth/profile

Tracker: `POR01` complete; `POR02` active. Rollup: **88 complete · 4 active · 200 not started · 0 blocked**.

Official identity correction:

- The requested `glm-4.3-flash` spelling is not present in the official OpenRouter catalog. The current route is `z-ai/glm-5.3-flash`, released 2026-08-26.
- OpenRouter publishes the current route/capability row, and Z.ai's release/developer documentation confirms `ox-alpha` was the anonymous pre-release identity. POR01 changes the provider default; POR02 owns exact live catalog normalization and strict activation.

Outcome:

- Added crate/plugin `heycode-provider-openrouter` / `provider-openrouter`. It owns safe OpenRouter profile metadata and exact `authorization_flow:openrouter-api-key` registration as a Context effect.
- The plugin retains the selected `CredentialQuery`, uses replaceable masked prompt/validator providers in tests and constructs the official `/api/v1/key` plus optional `/api/v1/models` validation plan in production.
- Invalid-key validation fails with the stable `unauthorized` flow class before credential commit. Shutdown removes the flow even from a held authorization registry.
- The shared `authorization-api-key` plugin now contributes DeepSeek only. Exact production inventory proves the OpenRouter row has one owner: `provider-openrouter`.
- `OpenRouterProvider::DEFAULT_MODEL` and setup/profile metadata now use `z-ai/glm-5.3-flash`.
- Config schema v12 adds redacted semantic change `MoveAuthorizationFlowToProviderPlugin`. Historical exact profiles containing `authorization-api-key` receive `provider-openrouter` immediately after it; unrelated rows remain byte-semantically preserved, an existing new row is idempotent, and generated built-in profiles naturally receive the current default set.
- The shared workspace's externally added test-binary consolidation was preserved and formatted. The pre-POR01 full baseline remained **817 tests / 52 non-empty suites** green.

TDD evidence:

- Red: the new provider crate tests could not import any OpenRouter profile/plugin contract.
- Red: schema-v11 migration tests could not name the provider-flow ownership change.
- Red: real composition did not contain `provider-openrouter`, and the OpenRouter flow remained attributed to the shared plugin.
- Green: provider profile/default/query, invalid-key-before-write, effect disposal, semantic migration ordering, production plugin presence and exact inventory ownership.

Primary code:

- `crates/heycode-provider-openrouter/`
- `crates/heycode-llm/src/openrouter.rs`
- `crates/heycode-config/src/{migration,migration_doctor}.rs`
- `crates/heycode-doctor/src/model.rs`
- `crates/heycode-cli/src/{lib,setup}.rs`
- `crates/heycode-cli/tests/it/provider_openrouter.rs`

Focused verification:

- `cargo fmt --all` — pass.
- `cargo clippy -p heycode-provider-openrouter -p heycode-config -p heycode-llm -p heycode-cli --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-provider-openrouter -p heycode-config -p heycode-llm -p heycode-cli` — **223 focused tests passed**; workspace inventory is now **821 tests / 53 consolidated non-empty suites**.

Next boundary:

- POR02 extends the provider plugin with operation-time credentialed OpenRouter `/models` discovery, all-or-nothing schema validation and current GLM-5.3-Flash capability normalization, then supplies the exact evidence required to expose strict `InferenceAdapter` dispatch.

## 2026-08-27 — POR02 OpenRouter full/singular live catalog

Tracker: `POR02` complete; `POR03` active. Rollup: **89 complete · 4 active · 199 not started · 0 blocked**.

Official wire evidence:

- Complete text-model discovery is public `GET https://openrouter.ai/api/v1/models` and returns a non-paginated root `{data,total_count,links.next}` when no pagination arguments are supplied.
- Exact lookup is singular `GET /api/v1/model/{author}/{slug}`. The initially guessed plural single-row path returns 404; official docs and a live request confirm the singular route.
- The 2026-08-27 live catalog contained 417 rows. The exact `z-ai/glm-5.3-flash` row reports model context 1,310,720, current top-provider context 1,048,576, output 131,072, text/image/video input, mandatory reasoning with max/high/low effort, tools, structured output and cache-read pricing.

Outcome:

- Added effect-owned `catalog-openrouter` in `heycode-provider-openrouter`, mounted after catalog persistence/DeepSeek discovery in production and in the restricted setup world.
- Each refresh validates the complete list and count before issuing exact GLM lookup. Empty/partial/paginated/count-mismatched generations perform no detail request and publish nothing.
- Full and exact GLM rows must agree on every selected field before publication. A missing/mismatched default rejects the complete candidate.
- Model ids/canonical slugs, numeric limits, modalities and supported parameters are bounded and strict. Provider display names trim surrounding whitespace; multiline descriptions allow only ordinary whitespace and remain capped. Empty supported-parameter arrays are explicit lack-of-support evidence, not corruption.
- Known list fields normalize tools, reasoning, image input and structured output to Supported/Unsupported. Cache-read pricing proves prompt-cache support; native web/compaction remain Unknown for later provider tasks.
- Descriptor limits use the conservative current top-provider values rather than the larger theoretical model maximum. Non-null official expiration dates become future Deprecated deadlines exactly as the OpenRouter schema defines.
- Setup/provider discovery now sees both DeepSeek and OpenRouter catalog sources without making a request during composition. Strict OpenRouter inference remains disabled until POR03/POR04 resolve routing and mandatory reasoning/tool wire semantics.

TDD/live evidence:

- Red: catalog types/plugin were absent.
- Red: production composition lacked `catalog-openrouter` and `model_catalog:openrouter` ownership.
- The first live canary rejected valid multiline descriptions/trailing display whitespace; field-specific normalization replaced the overbroad string rule.
- The second live canary rejected three valid router rows with empty `supported_parameters`; deterministic fixtures now pin empty-as-explicit-unsupported while duplicates remain invalid.
- Green: full+detail normalization, partial/status failure classes, effect disposal, production inventory and the safe unauthenticated official catalog canary.

Primary code:

- `crates/heycode-provider-openrouter/src/catalog.rs`
- `crates/heycode-provider-openrouter/tests/it/catalog.rs`
- `crates/heycode-cli/src/{lib,setup}.rs`
- `crates/heycode-cli/tests/it/{provider_openrouter,setup_catalog,composition,plugin_inventory}.rs`

Current verification:

- Deterministic POR02 catalog tests — pass.
- `HEYCODE_E2E=1 cargo test -p heycode-provider-openrouter live_official_catalog_matches_the_glm_fixture_when_enabled` — pass without a credential.
- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace --no-fail-fast` — **825 passed / 53 consolidated non-empty suites**.
- Explicit temporary-home doctor — healthy, **4 passed / 0 warning / 0 failed / 0 skipped**.
- Explicit temporary-home fake headless run — `[done:stop]`.

Next boundary:

- POR03 models OpenRouter order/fallback/required-parameter/ZDR/data-policy choices as resolved route data that reaches the wire. POR04 then binds GLM-5.3-Flash's mandatory reasoning effort/default and tool semantics before `Provider::inference_adapter` may return `Some`.

## 2026-08-27 — POR03 durable OpenRouter provider routing

Tracker: `POR03` complete; `POR04` active. Rollup: **90 complete · 4 active · 198 not started · 0 blocked**.

Outcome:

- Added core `ProviderRequestOption`: provider/kind/schema-v1 identity, object-only recursively bounded data, safe serde and data-redacted Debug.
- Threaded provider options through `RequestDraft`, private `ResolvedCall`, C02 `request/header.options`, v2 read compatibility and C05 exact durable/live comparison. Older headers default the list empty.
- `Provider::request_options` lets the selected provider propose policy before adapter resolution; the Agent includes those values only on the strict verified path.
- Added typed `OpenRouterRoutingPolicy` for exact provider order, fallback permission, required-parameter enforcement, `allow|deny` data collection and optional per-request ZDR. Provider slugs are bounded unique lowercase components.
- OpenRouter materializes documented defaults explicitly: fallbacks true, parameter enforcement false, data collection allow, no order and no ZDR restriction.
- Chat config binds one provider option kind to one non-reserved top-level wire field. OpenRouter maps `routing` to `provider`; no shared adapter provider-name branch exists. Responses/Messages and unconfigured Chat routes reject nonempty options before transport.
- A configured Chat dialect permits an absent option for direct conformance and pre-cancellation. Unsupported nonempty kinds remain fail-loud.

TDD/evidence:

- Red: core request-option vocabulary did not exist.
- Red: `RequestDraft` had no provider-options plane, so durable snapshot verification could not represent routing.
- Red: OpenRouter had no typed routing policy, constructor or Provider option hook.
- Green: safe option serde/debug/bounds, request-header round trip, C05 option mutation rejection, exact Chat `provider` JSON and invalid order refusal.
- Focused package run found the configured-but-absent dialect masked cancellation as Protocol; the existing cancellation regression now pins the compatibility arm.

Focused verification:

- Targeted clippy for core/session/LLM/agent — pass after one diagnosed style fix.
- Core, session, LLM and agent suites — **287 tests pass** (one existing cancellation regression failed, was diagnosed/fixed, then passed).
- Workspace inventory: **829 tests / 53 consolidated non-empty suites**. Full gate follows POR04 strict activation.

Primary code:

- `crates/heycode-core/src/vocab.rs`
- `crates/heycode-llm/src/{provider,inference,chat,openrouter,responses,anthropic}.rs`
- `crates/heycode-session/src/request.rs`
- `crates/heycode-agent/src/{agent,request_invariant}.rs`
- focused core/session/LLM/agent tests and `heycode-session/README.md`

Next boundary:

- POR04 configures GLM-5.3-Flash as mandatory enabled reasoning with exact max/high/low effort ids, validates tool-state replay/response output and only then advertises `Provider::inference_adapter` for production verified dispatch.

## 2026-08-27 — POR04 deterministic strict GLM activation (live evidence pending)

Tracker: `POR04` remains active. Implementation is complete; its acceptance still requires a trustworthy authenticated live reasoning/tool artifact.

Outcome:

- OpenRouter now advertises `Provider::inference_adapter`. GLM-5.3-Flash accepts exact max/high/low and materializes adapter-default max through OpenRouter's `reasoning.effort` object.
- Chat preserves streamed `reasoning`, `reasoning_content` or exact ordered `reasoning_details` objects in lossless provider state and replays the original form. Alias switching/both-at-once and unsafe/oversized details fail.
- GLM assistant tool calls require nonempty raw reasoning or a complete detail sequence on both projected input and response settlement. Missing state emits neither provider state nor successful Finish. DeepSeek's `reasoning_content`-only contract remains separate.
- Schema v13 conditionally inserts `catalog-openrouter` before `llm` only for older exact configs whose selected provider is OpenRouter, then materializes required `http` and `models` rows. Other provider profiles retain exact intent.
- A production-loader test seeds the durable GLM catalog, uses a real OpenRouter provider over a recording transport, runs Agent.send through strict catalog→resolve→snapshot→project→verify→dispatch and proves durable routing/default reasoning provenance.

TDD evidence:

- Red: OpenRouter did not advertise its adapter or default a reasoning effort.
- Red: a schema-v12 exact OpenRouter profile did not plan any strict-catalog migration.
- Green: max default, medium refusal, reasoning-details tool replay, missing ingress/egress refusal, schema-v13 conditional migration and verified production-loader dispatch.

Current verification:

- POR04-specific LLM tests — pass.
- Schema-v13 migration test — pass.
- Production-loader strict OpenRouter dispatch test — pass.
- `cargo fmt --all --check` — pass.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace --no-fail-fast` — **835 passed / 53 consolidated non-empty suites** after four diagnosed compatibility failures were fixed and individually rechecked.
- Explicit temporary-home doctor — healthy, **4 passed / 0 warning / 0 failed / 0 skipped**.
- Explicit temporary-home fake headless run — `[done:stop]`.

Evidence gap:

- No valid isolated OpenRouter credential is available to heycode on this host. The process-global dummy Keychain row may shadow the file fallback and previously produced an untrustworthy 401. No token files/values are inspected or copied. POR04 stays `[~]`; implementation can continue independently while the owner resolves/supplies credential authority.

Next implementation boundary:

- Start N01 logical native-tool registry/router while POR04 awaits external live authority. Provider-native selection must be committed in the same request header before N02 normalizes server-tool events.

## 2026-08-27 — N01: logical native-tool registry and durable route provenance

Tracker: `N01` complete; `N02` active. Rollup: **91 complete · 5 active · 196 not started · 0 blocked**.

Outcome:

- Added crate/plugin/service `heycode-native-tools` / `native-tools`. Provider, client and MCP candidates register as Context effects with validated logical/implementation/provider identities and token-owned disposal.
- Deterministic resolution groups by logical id, selects a matching provider-native candidate before client then MCP, and breaks same-family ties by descending priority then stable implementation id. Provider-native rows owned by another selected provider are ineligible.
- Added core `NativeToolRoute` and `NativeToolImplementationKind`. `RequestDraft` and private `ResolvedCall` retain the sorted route set; resolution rejects malformed, duplicate, unsorted or wrong-provider selections.
- `request/header.options.native_tool_routes` durably stores exact selections with an empty default for historical v2 lines. Session read validation and C05 live/durable comparison make the route choice replayable and transport-gating.
- The built-in web Consumers contribute `client:web_fetch` and `client:web_search` only when web is enabled. Exact `native_tool` inventory rows remain owned by `tools`; future provider/MCP plugins can contribute alternatives without a binary routing table.
- Production composition/factory/service/inventory audits include the new plugin. Schema v14 inserts it before every historical exact `tools`, `agent` or `subagent` consumer while preserving unrelated custom-profile intent.

TDD and diagnosis:

- Red: the strict production OpenRouter composition test could not compile because the durable request snapshot had no native route field; after adding the field it asserted both selected client web implementations across the complete registry→resolve→commit→project→verify path.
- Registry tests first pinned provider matching, client fallback, deterministic ordering, duplicate refusal, invalid ownership, durable serde and shutdown disposal.
- The first focused run found one trimmed CLI profile still omitted the new mandatory service. The remaining focused batch exposed eleven schema-history expectations that correctly received the new dependency but still asserted schema-13 arrays. Only those exact fixtures changed; no production algorithm was weakened.

Verification:

- `cargo fmt --all --check` — pass.
- Targeted all-target clippy with warnings denied across core/native-tools/tools/session/LLM/agent/MCP/skills/status/config/CLI — pass.
- Touched-crate suites — **501 unique tests passed** after the diagnosed fixture repairs.
- Current source inventory — **839 tests / 54 consolidated non-empty suites**. The last full workspace/product-smoke milestone remains POR04 at 835/53; no new full-gate claim is made for N01 alone.

Next implementation boundary:

- N02 adds durable provider server-tool call/result/citation events, protocol-correct replay and UI-safe normalized projections. N01 also unlocks `POR05` OpenRouter web search and `N04` explicit native/local policy modes; those stay separate so route choice cannot be mistaken for provider event semantics.

## 2026-08-27 — N02: durable server-tool traces and exact-state continuation

Tracker: `N02` complete; `POR05` active. Rollup: **92 complete · 5 active · 195 not started · 0 blocked**.

Primary-source boundary:

- Current Anthropic server-tool documentation confirms `server_tool_use` calls, same-assistant `*_tool_result` blocks paired by `tool_use_id`, mixed client/server deferred settlement and `pause_turn` replay of the complete assistant response. Web search results contain public URL/title plus opaque `encrypted_content`; citations contain URL/title/cited text plus opaque `encrypted_index` and must be shown to users.
- Current OpenAI Responses evidence uses hosted-tool output items and URL/file annotations; OpenRouter standardizes web citations as `url_citation`. N02 freezes a generic safe vocabulary without claiming either provider's shipping activation. Their exact wire mappings remain provider-owned tasks.

Outcome:

- Added core `ServerToolCall`, `ServerToolResult`, `ServerToolSource`, `ServerToolOutcome` and `UrlCitation`. Calls require validated ids/logical/provider names and bounded object input. Results carry only outcome, optional count, safe error code and public HTTP(S) sources. Citation URL userinfo/non-HTTP schemes, unsafe text/ranges and oversized/deep data fail. Debug redacts call input, URLs and cited excerpts.
- Added `InferenceEvent::{ServerToolCall,ServerToolResult,Citation}`. Anthropic Messages emits completed server calls/results and URL citations while preserving the complete original message—including encrypted fields—as `ProviderStateItem`.
- Added v2-only `server-tool/call`, `server-tool/result` and `assistant/citation`; the v1 gate remains frozen. `ProjectedRequest.server_tool_events` retains safe chronological output while exact provider state alone rebuilds model input.
- Request projection validates producing request/turn/step, non-regressing provider output index, session-wide insert-once call ids, settle-once results and same-route later-request settlement. Orphan, duplicate, cross-route and malformed events fail.
- Agent buffers the complete exact+normalized group until terminal Finish, constructs a candidate log and runs the shared projection before the first append. Stream failure/cancellation or invalid correlation commits none of the group.
- `FinishReason::Pause` now auto-continues in a fresh durable request/step only for strict adapter streams that committed validated exact state. The Anthropic parser seeds pending server calls from replayed state and requires later result correlation. Legacy/state-free Pause fails, client calls cannot coexist with Pause and nine consecutive Pause responses terminate after the eight-continuation cap.

TDD and diagnosis:

- Red: core/session tests could not compile before the normalized values, three event kinds and projected request field existed.
- Red: the old Agent test required every provider Pause to fail; strict adapter tests now prove valid stateful continuation while the legacy test locks fail-closed state-free behavior.
- Red: the old Anthropic continuation fixture returned final text without settling its earlier server call. It was corrected to the official result-then-text shape; the parser remains strict rather than accepting unresolved state.
- A malicious-adapter fixture proves an orphan result plus otherwise-valid provider state fails structural admission before either output event appends.

Focused verification:

- `cargo fmt --all --check` — pass.
- Targeted core/session/LLM/agent all-target clippy with warnings denied — pass.
- `cargo test -p heycode-core -p heycode-session -p heycode-llm -p heycode-agent` — **300 tests passed**.
- `cargo clippy --workspace --all-targets -- -D warnings` — pass.
- `cargo test --workspace --no-fail-fast` — **848 passed / 54 consolidated non-empty suites**.
- Isolated schema-14 doctor — healthy, **4 passed / 0 warning / 0 failed / 0 skipped**.
- Isolated fake headless run — `FAKE-REPLY: offline smoke response.` then `[done:stop]`.

Next implementation boundary:

- POR05 contributes exact `openrouter:web_search` request configuration, Chat server-tool/citation normalization and deterministic production replay fixtures through the N01/N02 boundaries. Authenticated live evidence remains separately constrained by the known local Keychain shadow.

## 2026-08-27 — POR05 deterministic OpenRouter web-search surface

Tracker: `POR05` remains active. Rollup remains **92 complete · 5 active · 195 not started · 0 blocked**.

Primary-source reconciliation:

- Current OpenRouter docs require model-invoked `tools:[{"type":"openrouter:web_search","parameters":{...}}]`; the older `plugins:[{"id":"web"}]` and `:online` paths are deprecated.
- The documented Chat response exposes nested `url_citation` annotations and aggregate `usage.server_tool_use.web_search_requests`. OpenRouter's maintained AI SDK parses those exact fields. Neither source documents a per-search id/query/result block on Chat.
- The tracker acceptance cannot be closed by inventing a synthetic call. POR05 stays active until trustworthy authenticated raw evidence reveals a documented exact call shape or an OpenRouter Responses route supplies an exact hosted-tool item.

Deterministic implementation:

- Added provider plugin `native-openrouter`, exact inventory/candidate `openrouter:web_search` and effect-owned lifecycle. It wins only for the OpenRouter route; DeepSeek retains client search.
- Agent route projection removes the losing client `web_search` tool, retains client `web_fetch`, resolves `NativeFeature::Web` and commits exact routes/native feature/client schemas before dispatch.
- Added data-driven Chat server-tool definitions, mandatory 1..=30 top-level budget, URL-citation response dialect and retry disablement for native work. Unconfigured annotations/usage, unsafe URLs/ranges, count-over-budget and late annotations fail without ProviderState/Finish.
- Added typed OpenRouter search policy with explicit heycode defaults: `auto`, 5 results, 3 uses, 15 cumulative results, 4,000 characters/result and 5 total server-tool calls. Official `url_citation` objects are preserved in exact Chat state and emitted as N02 citations.
- OpenRouter catalog rows now mark provider-scoped `native_web` Supported because OpenRouter documents any-model fallback even when the upstream model lacks native search.
- Schema v15 inserts `native-openrouter` after `native-tools` for historical exact OpenRouter profiles. Default plugin/descriptors/inventory and migration doctor share the production factory path.
- The production-loader test proves provider candidate precedence, client duplicate removal, durable header/native feature, exact request tool/budget/routing/reasoning, aggregate search evidence and durable citation projection for `z-ai/glm-5.3-flash`.

TDD and verification:

- Red: Chat config had no server-tool/citation dialect, provider crate had no native candidate and schema 14 had no ownership migration.
- First focused run found only five schema-14 expected arrays; each correctly received the provider-native contribution and any missing older strict-catalog prerequisites.
- `cargo fmt --all --check` — pass after one mechanical format.
- Targeted affected-crate all-target clippy with warnings denied — pass.
- Affected crate suites — **332 tests passed**.
- Current source inventory — **852 tests / 54 consolidated non-empty suites**. Last full milestone remains N01/N02 at 848/54.

Evidence gap and continuation:

- No valid isolated OpenRouter credential is available; the known process-global dummy Keychain row remains untouched. No live request is attempted or interpreted.
- Continue an independent actionable lane while POR04/POR05 await trustworthy authority/observable call evidence. N04 policy modes are a natural continuation over the now-real provider/client alternatives.

## 2026-08-27 — N04: live native/local policy modes

Tracker: `N04` complete; `WEB01` active. Rollup: **93 complete · 6 active · 193 not started · 0 blocked**.

Outcome:

- Added immutable `NativeToolPolicy` and exact modes `prefer-native`, `prefer-local`, `native-only`, `local-only`, with a default plus at most 128 validated per-logical overrides.
- Registry ranking now treats only a candidate owned by the selected provider as native. Local ordering is client then MCP; priority/id remain deterministic inside a family. Prefer modes cross families. Only modes return stable errors naming the safe logical id/mode; unknown override ids fail rather than waiting for accidental future activation.
- Added optional default plugin `native-tool-policy`, which injects Settings plus the registry, owns Settings namespace `native-tools`, applies initial resolved state and watches committed live generations. Failed application becomes sticky-unavailable; shutdown removes the watcher then restores prefer-native.
- Agent already resolves the registry before request construction, so a live policy change coherently changes native route provenance, client tool schemas, `NativeFeature` selection and provider wire without restart.
- The production OpenRouter composition test switches from provider web search to `prefer-local`, verifies client `web_search` returns and the server tool/cap disappear, then applies `web_fetch:native-only` and proves a named refusal before request header/transport.
- No config schema bump: the policy plugin is an optional capability in exact custom profiles, while omitted `[profile]` worlds receive it through the built-in profile. Profiles that omit it retain N01's documented prefer-native default.

TDD and verification:

- Red: the registry exposed one hard-coded precedence and no Settings contribution/policy vocabulary.
- One exact inventory expectation advanced from two to three Settings namespaces; one generated-profile preview added the new default plugin. No production algorithm was weakened.
- `cargo fmt --all --check` — pass.
- Affected all-target clippy with warnings denied — pass.
- Focused native-tools/agent/CLI/config/OpenRouter suites — pass; current source inventory is **855 tests / 54 consolidated non-empty suites**.

Next implementation boundary:

- WEB01 extracts a provider-independent search/fetch Service Definition from the current client tools. N03 then contributes portable providers and equivalence fixtures through that seam.

## 2026-08-27 — WEB01/N03: replaceable web seam and portable provider

Tracker: `WEB01` and `N03` complete; `WEB02` active. Rollup: **95 complete · 6 active · 191 not started · 0 blocked**.

Outcome:

- Added 39th crate `heycode-web`, service/plugin `web`, exact contribution namespace `web_provider` and effect-owned `WebRegistry`. Provider descriptors declare search/fetch/priority; dispatch orders priority then id, carries one caller token, revalidates output and becomes terminal on shutdown.
- Added bounded/redacted search/fetch request/result vocabulary. Queries, URLs and bodies do not appear in Debug/errors. Public HTTP(S)/userinfo, result count, title/snippet/content/type and fetch caps fail at both construction and provider-return boundaries.
- Rewrote `heycode-tools` web built-ins as pure Consumers. A source-law test rejects reqwest, environment, DNS and credential access; provider errors map to stable model recovery text. `ToolsConfig` no longer carries a Brave endpoint.
- Added `web-portable` provider: operation-time `BRAVE_API_KEY` or keyless DuckDuckGo Lite, one normalized result contract, capped streaming response bodies, HTML/plain fetch and cancellation. Trusted provider endpoints are configurable for deterministic tests.
- Initial SSRF admission parses typed URL hosts, rejects private/loopback/unspecified IPv4/IPv6 literals, and denies a domain if any DNS answer is private. The first migrated test caught bracketed `[::1]` bypassing string IP parsing; typed `Host` matching fixed it.
- Default production graph mounts `web`/`web-portable` before web-enabled `tools`; schema v16 repairs historical exact profiles only when `[web].enabled` is not false. Core/CLI service/plugin/inventory audits include service `web` and `web_provider:portable`.
- Native fallback equivalence is covered at three layers: N04 production routing restores the client tool, the model tool dispatches only through an injected fake provider, and a local HTTP fixture proves Brave-or-DDG wire forms normalize identically.

TDD and verification:

- Red: the new crate tests could not import any web service/registry/types; tool source still contained HTTP/env/DNS implementation.
- One focused failure exposed the IPv6 bracket issue; ten schema snapshots then advanced mechanically to the new required dependency rows.
- `cargo fmt --all --check` — pass after one mechanical format.
- Affected all-target clippy with warnings denied — pass.
- Affected suites — **290 tests passed**; source inventory is **864 tests / 55 consolidated non-empty suites**. Last full milestone remains 848/54.

Known boundary and next task:

- The portable client uses a five-hop reqwest redirect policy, but only the initial URL/DNS authority is checked. WEB02 replaces implicit following with explicit per-hop DNS/private/rebind admission before any redirected request.

## 2026-08-27 — WEB02: redirect-aware, DNS-pinned SSRF admission

Tracker: `WEB02` complete; `WEB04` active. Rollup: **96 complete · 6 active · 190 not started · 0 blocked**.

Outcome:

- Replaced automatic fetch redirects with a no-redirect, manually admitted chain. Relative and absolute locations must remain HTTP(S), contain no userinfo, avoid prior URLs and stay within five hops.
- Every domain hop resolves exactly once through the caller cancellation token. Empty or any-private answer sets fail; admitted addresses are normalized to the effective port and pinned into the reqwest connection, closing the check/connect DNS-rebinding window.
- Typed IPv4/IPv6 literals, IPv4-mapped IPv6, loopback, link-local, unspecified, private, carrier-grade NAT, metadata and local/internal names fail before a request. Direct and redirected metadata fixtures never invoke DNS.
- Disabled automatic search redirects. Trusted provider endpoint configuration does not grant authority to forward Brave's custom credential header to a new origin.
- Replaced a `spawn_blocking` resolver handle that detached on cancellation with a directly selectable Tokio resolver future. A source-law test locks the no-detached-handle invariant.

TDD and verification:

- Red: the credential-redirect fixture followed the 302 into the sink and surfaced `Network` rather than the expected initial-hop `Http` refusal.
- Green: the same fixture observes no sink connection; mixed DNS, pinned single resolution, metadata redirect, unsafe location, loop, hop cap and mapped IPv6 cases pass.
- `cargo fmt --all --check` — pass after one mechanical format correction.
- `cargo clippy -p heycode-web --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-web` — **10 passed**, 0 failed. Current source inventory is **868 tests / 55 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Next implementation boundary:

- WEB04 makes search/fetch provider and domain policy explicit, Settings-backed and visible. WEB03 remains dependency-blocked on ATT01 rather than absorbing unowned attachment/PDF storage semantics.

## 2026-08-27 — WEB04: explicit provider/domain policy and `/web` visibility

Tracker: `WEB04` complete; `ATT01` active. Rollup: **97 complete · 6 active · 189 not started · 0 blocked**.

Primary evidence:

- The existing DeepSeek Harness web seam pins `searchProvider` and `fetchProvider` independently and auto-selects only one usable implementation; multiple usable providers are an explicit ambiguity rather than a registration-order winner.
- Current OpenRouter server-tool search documents `allowed_domains`/`excluded_domains` with engine/native-provider compatibility differences; server-tool fetch documents allowed/blocked domains. OpenAI's native search schema documents allow-only filtering with subdomain inclusion. The portable client seam can enforce one exact policy itself, while provider-native adapters must map or refuse their own evidenced subset.

Outcome:

- Removed web-provider priority dispatch. Each search/fetch operation now uses its configured safe id or exactly one locally available capable provider. Reports distinguish selected/configured, selected/automatic, unavailable, configured missing/unsupported/unavailable and sorted ambiguity.
- Added cheap local `available()` evidence to providers and a bounded safe `WebCapabilityReport` containing registered capabilities, availability, both selections and both policies. Provider callbacks run outside the registry lock; panic becomes stable unavailability.
- Added `WebDomainPolicy`: canonical ASCII base domains, exact-or-subdomain matching, FQDN trailing-dot normalization, dot-boundary suffix safety, block precedence, duplicate/conflict refusal and 128-rule-per-list bounds. Debug retains counts rather than domain values.
- Added optional default plugin `web-policy` and live Settings namespace `web` with independent `search_provider`, `fetch_provider`, `search_domains` and `fetch_domains`. Persisted ids validate against the composed per-operation provider catalog. Watchers update only after Settings persistence/publication; shutdown removes the watcher then restores auto/allow-all.
- One immutable domain-policy snapshot travels with each request. Search validates provider output before filtering disallowed URLs. Fetch refuses the initial URL before provider execution, portable fetch repeats policy on every redirect, and `WebFetchResult` now carries a redacted validated final URL that the registry checks independently.
- Added optional command plugin `status-web`, exact command `/web` and explicit `web` injection. It renders provider capability/availability, effective selections and bounded domain lists. It remains separate from `status`, avoiding a hidden optional service lookup or order-sensitive capture.
- Default composition/factory/generated-profile truth includes `web-policy` and `status-web`; intentional exact profiles may omit them and retain the original unique-auto/allow-all behavior. This optional addition does not change root config schema 16.

TDD and verification:

- Red: WEB04 types/plugin/availability did not exist. After implementation, the exact production inventory correctly failed because Settings namespaces rose from three to four; the audit was updated and its fully-qualified test rechecked.
- `cargo fmt --all --check` — pass after mechanical formatting.
- Warnings-denied all-target clippy — pass for `heycode-web`, `heycode-tools`, `heycode-status`, `heycode-config` and `heycode-cli`.
- Affected inventory — **198 tests green** across web (14), tools (77), status (5), config (45) and CLI (57), using the first CLI run's 56 unaffected passes plus the corrected exact-inventory recheck rather than rerunning them blindly. Current source inventory is **873 tests / 55 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Next implementation boundary:

- ATT01 establishes content-addressed attachment bytes plus durable safe metadata through the capability-rooted filesystem/session laws. It unlocks WEB03 rich HTML/PDF extraction, ATT02 image input, MCP12 rich results, X03 ACP media events and later provider multimodal slices.

## 2026-08-28 — ATT01: immutable attachment bytes and durable metadata

Tracker: `ATT01` complete; `WEB03` active. Rollup: **98 complete · 6 active · 188 not started · 0 blocked**.

Primary evidence:

- The maintained image-rs 0.25.10 `ImageReader::with_format(...).into_dimensions()` path reads dimensions through a selected decoder without constructing a full decoded pixel buffer. heycode still owns byte/pixel/MIME/hash policy around that parser.
- ATT01's durable law is bytes first and metadata second. `attachment/added` cannot contain inline base64 or enter model/runtime projection until ATT02/X03 binds an evidenced route.

Outcome:

- Added 40th crate `heycode-attachments`, service key `attachments` and optional default plugin `attachments-local`. It injects the active session and receives explicit isolated root/max-byte inputs from every production/test composition boundary. Default admission is 32 MiB; the universal typed ceiling is 64 MiB. The exact service registry is now 35 keys and the default plugin order 57 rows.
- Added core `AttachmentContentId`, `AttachmentMediaType`, `AttachmentDimensions` and `AttachmentMetadata`. SHA-256 ids are canonical lowercase validated newtypes. MIME has strict parameter-free token syntax. Names are bounded basename-like labels. Raster dimensions are nonzero, at most 32,768 per side and 100 million pixels. Debug redacts content ids/names.
- Added closed v2 event `attachment/added`; v1 keeps its frozen known-kind set and rejects the new tag. Session query treats an attachment as activity and may match its safe display name. Neutral/model and native-runtime projections explicitly ignore metadata until ATT02/X03.
- Admission content-sniffs PNG/JPEG/GIF/WebP, PDF, UTF-8 text or opaque binary. An optional claimed MIME must match. Images use header-only dimension reads; empty/oversized/mismatched/corrupt/unsupported input fails before storage or event publication.
- The Unix provider opens an owner-only schema-v1 root and fixed `0600` lock. `0700` object directories and `0600` single-link files are validated. Same-process ownership plus cross-process `flock` serializes object publication. Exact bytes fsync to a temporary, hard-link no-clobber to the digest path, unlink, and directory-fsync. Existing objects must byte-match.
- Reads verify file type/link/mode/identity/length before/after, then SHA-256, sniffed MIME and dimensions against durable metadata. Tampering cannot silently become model input later.
- One active-operation lease owns each synchronous admission/read. Shutdown cancels first then waits for all leases; no task handle exists to detach. Session publication occurs after the byte commit and supports a reentrant listener reading the object during the append callback. Cancellation/session failure may leave an unreachable immutable object but never a phantom event.
- Non-Unix local storage returns `UnsupportedSecurity`. Linux and Windows target checks compile the respective audited/unsupported shapes; neither cross-check is substituted for native Windows security evidence.

TDD and verification:

- Red: core attachment types, service/plugin and event kind did not exist. The closed enums then forced explicit projection decisions.
- One v1 refusal fixture initially wrote directly beneath a random temp-root basename and correctly hit `UnsafePath` before kind validation. Moving the same bytes under a valid session directory reached and passed the intended `UnknownKind` contract; the other 279 passing tests were not rerun.
- `cargo fmt --all` — pass.
- Warnings-denied all-target clippy — pass for core/session/attachments/agent/config/CLI.
- Affected inventory — **280 tests green**: core 36, session 83, attachments 4, agent 54, config 45 and CLI 58. Current source inventory is **882 tests / 56 consolidated non-empty suites**; last full workspace milestone remains 848/54.
- `cargo check -p heycode-attachments --target x86_64-unknown-linux-gnu` — pass.
- `cargo check -p heycode-attachments --target x86_64-pc-windows-gnu` — pass with the deliberate unsupported-security backend.

Dependency impact and next boundary:

- ATT02 and X03 are newly actionable. WEB03 is now active and supplies bounded HTML/PDF extraction with durable source metadata; completing it will unlock ATT03 native-vs-extraction document policy and WEB05 untrusted-content annotations.

## 2026-08-28 — WEB03: bounded HTML/PDF extraction and durable citations

Tracker: `WEB03` complete; `ATT02` active. Rollup: **99 complete · 6 active · 187 not started · 0 blocked**.

Primary evidence:

- lopdf 0.44 exposes bomb-safe `LoadOptions::max_decompressed_size`, bounded page content and bounded text extraction. Its maintained line includes the nested-object recursion fix introduced in 0.42.
- html2text 0.17.1 renders an `io::Read` HTML source to wrapped plain text and links. heycode remains responsible for source/output limits, lifecycle and durable provenance.

Outcome:

- Added exact contribution namespace `web_processor`, effect-owned processor registration and sorted report visibility. Matching uses zero→provider fallback, one→run, multiple→ambiguous; registration order never wins. Portable provider holds only a weak processor handle, avoiding a registry→provider→registry Arc cycle.
- Added optional default `web-extract` after `web-portable`; it injects both `web` and `attachments` and contributes `web_processor:portable-readable`. Intentional exact profiles may omit it with no schema bump.
- `WebFetchRequest` now distinguishes raw-source and readable-output ceilings. `web_fetch` resolves 4 MiB raw and 64 KiB readable limits explicitly. Registry/provider boundaries independently reject over-limit output even when marked truncated.
- Added redacted `WebRawDocument`, structured `WebFetchSource` and source-aware `WebFetchResult`. Provenance includes final URL, bounded title, retrieval milliseconds, raw-truncation state, optional content id and PDF page count.
- HTML uses html2text at width 100, removes script/style execution text, retains links and extracts a bounded title. Plain UTF-8 remains a conservative fallback. Unknown textual media stays in provider fallback; unsupported binary is no longer lossy-decoded.
- PDF loads with a 2 MiB decompression ceiling, at most 256 pages, bounded per-page text chunks and UTF-8-safe total output; `[Page N]` markers make page references stable. Encrypted/malformed/raw-truncated/>2 MiB page content fails before source admission.
- Extraction runs in one owned blocking task. Caller/plugin/10-second cancellation reaches bounded PDF work, and every branch awaits the handle. Timeout therefore settles rather than detaches.
- A durability audit moved source provenance into core `AttachmentSourceMetadata`. Successful parsing constructs one typed URL/title/time/truncation/page record, then ATT01 commits the exact raw bytes and that provenance in `attachment/added`. `WebFetchSource` is constructed from the same facts. Failed extraction creates no attachment event.
- `web_fetch` renders an escaped Markdown source link and PDF page count, never the content hash. The durable tool result remains replayable while U17/WEB05 own structured cards/untrusted labels.
- Production composition mounts/reports the processor and performs a real HTML extraction through the factory/loader, with matching durable attachment provenance.

TDD and verification:

- Red: processor/document/source APIs and plugin did not exist. The first focused HTML/PDF fixtures then passed once the boundary was implemented.
- `cargo fmt --all` — pass.
- Warnings-denied all-target clippy — pass across core/session/attachments/web/tools/status/config/CLI after one `sliced_string_as_bytes` mechanical correction.
- Affected inventory — **326 tests green**: core 36, session 83, attachments 4, web 18, tools 77, status 5, config 45 and CLI 58. Current source inventory is **886 tests / 56 consolidated non-empty suites**; last full workspace milestone remains 848/54.
- Extra `heycode-web` Linux/Windows cross-check was attempted once and stopped in `ring`'s build script because neither target C compiler exists on this Mac. No heycode target code compiled, so this is recorded as external toolchain-blocked, not a pass or product blocker.

Dependency impact and next boundary:

- ATT02 image input/projection is active. ATT03 document route policy and WEB05 untrusted-content annotations are newly actionable; X03 remains actionable from ATT01.

## 2026-08-28 — ATT02: durable image selection and exact provider projection

Tracker: `ATT02` complete; `ATT03` active. Rollup: **100 complete · 6 active · 186 not started · 0 blocked**.

Primary evidence:

- OpenAI Responses defines `input_image` with URL or base64 data URL and explicit `detail`; Chat Completions defines `image_url` content parts with nested URL/detail.
- Anthropic Messages defines base64 `image` blocks for JPEG/PNG/GIF/WebP and recommends placing images before text. The implementation pins that ordering rather than treating “OpenAI-compatible” as a universal multimodal shape.

Outcome:

- Added bounded `ChatImage` bytes to the provider-neutral LLM vocabulary. Only PNG/JPEG/GIF/WebP and 1 byte..=32 MiB construct. Debug reports MIME/byte count, never content.
- Responses serializes user text then `input_image` data URLs; Chat serializes text then `image_url` parts; Anthropic serializes base64 image blocks before text. Non-user images and modality/content disagreement fail resolution before transport.
- Added v2-only `user/attachments`. One to sixteen unique exact prior `attachment/added` records must immediately precede their `user/message`. The specialized append writes both JSONL lines in one buffered append/flush, then emits both bus events. Resume, request projection, fork boundaries, compaction and query validate/preserve the pair; v1 remains frozen.
- Added `AttachmentStore::admit_image_path`. Explicit absolute regular-file selections refuse symlinks, recheck identity/length/timestamps around the bounded read and reject non-images before storage/session mutation.
- Agent now owns an effect-installed optional attachment binding. Plugin `agent-attachments` injects `agent`, `commands` and `attachments`, contributes exact `/attach <path...>`, and removes its command/binding under Context LIFO shutdown. Default composition grows to 59 plugin rows without a root schema bump.
- Before any user association, Agent checks count/uniqueness/image MIME, rereads and hash-verifies every object, constructs its exact protocol image, requires an advertised strict adapter, refreshes the selected model and accepts only `image_input=Supported`. Unsupported and Unknown are distinct named refusals. The request header records `input_modalities=[text,image]`, and independent C05 projection rereads the same bytes before dispatch.
- Cancellation covers the active turn and caller while image capability refresh is awaited. Cancelling one waiter writes no selection/message/draft/transport. The provider fetch itself remains the catalog registry's deliberate shared single-flight work and can satisfy another waiter; registry shutdown remains its lifecycle owner.
- TUI stages up to sixteen unique images, shows their count in the composer, retains them after preflight refusal and clears only on post-commit `UserAttachmentsEcho`. Live and replay transcript rows show safe name/MIME/dimensions immediately before the user text.
- Headless mode accepts repeatable `--image <path>` only with a one-shot prompt and admits each through the same store boundary. Setup, ACP, doctor and prompt-less interactive invocations refuse the flag.

TDD and verification:

- Red/exhaustiveness work forced every session/runtime/UI projection to decide the new kind. Exact wire tests pin all three protocol shapes; Agent tests pin successful durable/verified dispatch, unsupported/unproven refusal, command ownership and shared-refresh cancellation semantics; TUI/CLI tests pin staging/replay/rendering and argument admission.
- The first warnings-denied pass found test modules placed before production items and a complex attachment-slot type; both were structurally corrected. The first focused test run then exposed an incorrect test assumption that caller cancellation owns a shared catalog fetch. The corrected regression proves the documented registry owner and eventual shared cache fill. Exact production command order was repaired from the actual composed inventory, not changed to fit a stale expectation.
- `cargo fmt --all` — pass.
- Warnings-denied all-target clippy — pass across core/session/attachments/LLM/agent/TUI/config/CLI.
- Affected inventory — **478 tests green**: core 36, session 84, attachments 5, LLM 136, agent 58, TUI 55, config 45 and CLI 59. Current source inventory is **898 tests / 56 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Dependency impact and next boundary:

- ATT03 is active. It must choose provider-native document blocks only from exact model/protocol evidence and otherwise reuse WEB03's bounded extraction with explicit provenance; it must never silently rasterize/extract a document while claiming a native path.

## 2026-08-28 — ATT03: explicit native-versus-extracted document routing

Tracker: `ATT03` complete; `WEB05` active. Rollup: **101 complete · 6 active · 185 not started · 0 blocked**.

Primary evidence:

- OpenAI's current file-input guide defines native PDF/file content for both APIs: Responses `input_file` with filename + data URL, and Chat `file` with nested filename + data URL. PDF processing depends on model vision capability; protocol shape alone is not model evidence.
- Anthropic Messages defines base64 `document` blocks for `application/pdf`, a 32-MB request ceiling, page limits and no encrypted/password PDFs. Its shape is not interchangeable with OpenAI.

Outcome:

- Added independent tri-state `ModelCapabilities::document_input`, `InputModality::Document` and `RequestedCapability::DocumentInput`. Catalog-file schema-v1 reads old generations as Unknown and writes the new field. OpenRouter maps public `file` input modality separately; current GLM-5.3-Flash fixture remains explicit Unsupported rather than borrowing image support.
- Added bounded redacted `ChatDocument`: exact PDF MIME, safe basename, `%PDF-` signature and 1 byte..=32 MiB. Responses serializes `input_file`, current Chat serializes `file`, and Anthropic serializes a base64 `document` before text. All reject media on non-user messages; common resolution enforces one-to-four documents and a 32-MiB aggregate.
- Refactored WEB03 extraction into an effect-owned reusable `DocumentExtractor` service published by `web-extract`. Local inputs accept content-verified PDF/HTML/plain text up to 32 MiB and return bounded UTF-8/title/page/truncation without writing web provenance or an attachment. Web processing retains its existing raw-source commit. Both paths share html2text/lopdf bounds, panic containment, 10-second deadline and always-joined cancellation worker; held services stop after shutdown.
- Added `AttachmentStore::admit_document_path`, accepting only explicit content-sniffed PDF/HTML after the same symlink/identity/length race checks. Added optional default plugin `agent-documents`, which injects Agent/commands/attachments/document-extractor, effect-binds the extractor and owns immediate `/document <path...>`.
- CLI now preserves exact mixed media flag order through repeatable `--image` and `--document`; both remain one-shot-only. Default composition grows to 60 plugins and 36 service/seam keys without changing root schema 16; intentional exact profiles acquire no hidden capability.
- Added core `DocumentInputRoute::{Native,Extracted}`. Native requires identical PDF source/selected metadata. Extracted requires exact PDF/HTML source plus distinct `text/plain` selected metadata. `user/attachments` retains the backward-compatible empty route default, but every non-image selection now has exactly one validated route referencing prior admissions.
- Agent verifies all source objects before admission, refreshes the selected catalog once, and chooses native only for an advertised strict adapter plus exact Supported and bounded aggregate PDF bytes. Every Unsupported/Unknown/legacy/HTML route runs the composed extractor, durably admits its 64-KiB product as immutable text, and records source→selected Extracted. Live mapping and independent C05 projection reread the same selected object and render a deterministic document block; replay never re-decides from current catalog state.
- TUI generalizes the staged count to attachments and labels committed document rows `native document` or `locally extracted document`. Pending state still clears only after durable echo. Native/extracted notices publish only after the selection/message commit.

TDD and verification:

- Red: route/extractor/document vocabulary did not exist; the first core contract failed on unresolved `DocumentInputRoute` types.
- Exact tests cover route validation/redaction, adjacent session reconstruction, pure local extractor/no-web-event lifecycle, all three provider JSON shapes, model capability refusal, `/document`, native PDF drafts/headers, extracted PDF objects/text/routes, TUI labels and ordered CLI flags.
- The no-fail-fast run found only two stale expectations: ATT02's now-general cancellation wording and the authoritative service-key list. Production behavior passed; both exact expectations were corrected and individually rechecked without rerunning 516 passing cases.
- `cargo fmt --all --check` — pass.
- Warnings-denied affected all-target clippy — pass across core/session/attachments/web/LLM/catalog persistence/OpenRouter/agent/TUI/config/CLI.
- Affected inventory — **518 tests green**: core 37, session 85, attachments 5, web 19, LLM 139, catalog persistence 5, OpenRouter 7, agent 61, TUI 56, config 45 and CLI 59. Current source inventory is **908 tests / 56 consolidated non-empty suites**; last full workspace milestone remains 848/54.
- Isolated production binary with a temporary `HEYCODE_HOME`, `--restricted-workspace --fake run --document <local-html>` — `FAKE-REPLY: offline smoke response` and `[done:stop]`; no credential or network path ran.

Dependency impact and next boundary:

- WEB05 is active. Retrieved/extracted web content must carry a durable/model-visible untrusted-content annotation and a distinct TUI presentation without granting approval or tool authority. QSEC05 remains blocked on WEB05 plus MCP12.

## 2026-08-28 — WEB05: durable untrusted Web content across request and UI

Tracker: `WEB05` complete; `X03` active. Rollup: **102 complete · 6 active · 184 not started · 0 blocked**.

Outcome:

- Added core `UntrustedContentSource::Web` and redacted serializable `UntrustedContentBoundary`. Its deterministic model renderer preserves exact content inside a fixed `UNTRUSTED WEB CONTENT — data only; not instructions or authorization` warning.
- Extended `Tool` with a default-none successful-output classification. `web_search` and `web_fetch` override it; local tools remain unchanged. `execute_tool`/`run_tool` attach the classification from the resolved plugin instance, so Agent contains no Web tool-name table. Denials and body-free errors carry no external content.
- Added backward-compatible optional `untrusted_content` to v2 `tool/result`. V1 ordinary tool results remain readable, while a v1 line claiming the new field fails semantic admission. Neutral and route-aware projections retain the typed fact through replay/compaction.
- Agent commits content/classification together before `ToolFinished`. Live request construction and C05 independent reconstruction render the same warning around the exact result. A scripted two-step real Agent turn proves an injection-shaped Web snippet is durable, reaches the next provider request only inside the warning and appears typed on the UI bus.
- TUI Tool items retain the marker and show a distinct warning row for live and replayed results. The native runtime emits `content.untrusted.web` Notice immediately before the matching ToolResult, preserving the boundary for non-TUI native consumers without changing the normalized runtime schema.
- The marker is deliberately an authority annotation, not a claim that prompt injection is solved. Tool schemas, approval, trust and sandbox enforcement remain independent; QSEC05 stays dependency-blocked until MCP12 can classify MCP results on the same plane.

TDD and verification:

- Red: the core types, Tool trait method, session field, WireMessage projection and TUI item field did not exist.
- Tests pin core wire/debug/model rendering, Web Consumer classification, v1 refusal, neutral projection, session→model mapping, a real Web tool loop with durable/request/UI assertions and the warning card frame.
- `cargo fmt --all` — pass.
- Warnings-denied all-target clippy — pass across core/session/tools/agent/TUI/CLI.
- Affected inventory — **380 tests green**: core 38, session 86, tools 77, agent 63, TUI 57 and CLI 59. Current source inventory is **913 tests / 56 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Dependency impact and next boundary:

- X03 is active. ACP must expose tool/media/plan/usage events and cancellation over the existing normalized Agent/runtime planes without losing the new attachment routes or untrusted-content notice.

## 2026-08-28 — X03: ACP v1 rich content, normalized updates and cancellation

Tracker: `X03` complete; `X04` active. Rollup: **103 complete · 6 active · 183 not started · 0 blocked**.

Primary evidence:

- Current upstream ACP v1 defines prompt as an array of MCP-compatible content blocks; image and embedded resource require advertised capabilities. Session updates include user/agent/thought chunks, tool call/update, full plan and usage.
- `session/cancel` is a session notification and requires the original prompt response to settle with `stopReason=cancelled`; JSON-RPC `$/cancel_request` is a distinct protocol cancellation path.

Outcome:

- Replaced the sequential ACP dispatcher with one fair bounded JSONL server loop plus an owned prompt JoinSet. Prompt work no longer blocks input, so cancel notifications are handled while the model/tool operation is active. Frames are UTF-8/newline delimited and capped at 48 MiB, sufficient for one 32-MiB base64 media block without unbounded framing.
- Each ACP session now retains its exact composed Context, Agent, AttachmentStore, native RuntimeSession, runtime sequence cursor, active cancellation generation and owned approval supervisor. The former code dropped Context after `session/new` and detached the approval/writer tasks; EOF now cancels prompts, drains JoinSet handles, denies pending asks, joins approval, closes runtime and then unwinds Context.
- Extended `RuntimeInput` with validated unique durable attachment metadata and redacted Debug. The native RuntimeSession calls `send_with_attachments_cancellable`, so ACP media uses the same ATT02/ATT03 capability, extraction, logging and cancellation path.
- `initialize` now advertises image and embedded-context support, with audio/MCP false. Prompt accepts 1..64 official content blocks plus legacy string compatibility. Text/resource links are bounded into durable user text; image and embedded binary resource data base64-decodes under 32 MiB, content-validates and commits through ATT01. Client-supplied MCP servers/additional roots refuse instead of being ignored.
- User content chunks publish only after normalized `TurnStarted`, proving the user text/media commit succeeded. Runtime sequence filtering prevents replay duplication across later prompts. Agent/thought chunks, tool call/in-progress/update/result, plan replacement and provider-reported usage map to exact ACP v1 fields. WEB05's notice annotates tool updates through ACP `_meta`.
- `session/cancel` and `$/cancel_request` deny all pending interactive approvals, cancel the identity-matched prompt token and call RuntimeSession cancel. The original prompt returns `cancelled` only after durable Agent settlement. `InteractiveApproval::deny_all_pending` also makes frontend teardown fail closed.
- The ACP-only fake provider now reports deterministic 8/4 usage so the real binary test proves `usage_update` alongside text and embedded-resource user echo.

TDD and verification:

- Tests pin official initialize capabilities, rich resource/user/agent/usage stream, pure schema tool/plan/usage/untrusted mapping, bounded image admission, redacted RuntimeInput, cancelled runtime response and a duplex full server using a genuinely hanging provider.
- The first duplex teardown check correctly failed because dropping Tokio's split write handle did not half-close the stream. The test now calls `AsyncWrite::shutdown`; the unchanged server settles quiescently and the single failed case passes.
- `cargo fmt --all --check` — pass.
- Warnings-denied all-target clippy — pass across runtime/agent/CLI.
- Affected inventory — **143 tests green**: runtime 15, agent 64 and CLI 64. Current source inventory is **919 tests / 56 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Known boundaries and next task:

- X01 still needs a scripted real tool permission request/answer/resume proof; X02 owns strict absolute cwd, additional roots and route selection. X04 now builds the separate stable heycode app-server JSON-RPC v1 and a TUI-as-client path on these lifecycle primitives.

## 2026-08-28 — X04: stable heycode-local JSON-RPC v1 and TUI client

Tracker: `X04` complete; `X05` active. Rollup: **104 complete · 6 active · 182 not started · 0 blocked**. X04 also makes X05/X06 dependency-ready, moving two rows out of the blocked bucket before X05 becomes active.

Outcome:

- Added 41st crate `heycode-app-server`, service/plugin `app-server`, protocol version 1 and closed methods `initialize`, `session/open`, `turn/start`, `turn/cancel`, `session/close`. Closed `session/event` variants cover durable user input/attachment routes, turn lifecycle, assistant/reasoning, tools, usage, plan, notices and typed untrusted output.
- Every LocalAppClient request serializes to JSON-RPC and every response parses back. Every in-process notification serializes/deserializes through the stable wire before delivery, with an app-server-owned contiguous sequence. Invalid frames/methods/session ids and runtime errors map to fixed JSON-RPC classes rather than exposing internal/provider bodies.
- Added production NativeBackend over the composed Agent and native AgentRuntime. It lazily starts/resumes the exact current RuntimeSession, filters replay by runtime sequence, and reads the just-committed user text/attachments/document routes from the session log at TurnStarted. It maps normalized output and owns no task; the caller's future/channel/token own the operation.
- Added default plugin `app-server` immediately after `runtime-native` and exact service key `app-server`. TUI now injects the service and foreground non-command turns call `LocalAppClient`. Its existing supervised turn JoinHandle pumps stable events into AppState, cancellation uses the same caller token, and cleanup closes the client before Context teardown.
- During an app-client turn, duplicate direct user/assistant/reasoning/settlement UiEvents are filtered. Structured local tool cards, approval/dialog/status and model-scheduling command turns remain on their richer existing UiEvent path, avoiding regression to string-only diffs or duplicate plan/untrusted notices.
- Raised root config schema to 17. Older exact TUI profiles gain `app-server` immediately before TUI after existing runtime/routing dependencies; intentional profiles without TUI remain unchanged. Default composition is now 61 plugins and 37 service/seam keys.

TDD and verification:

- App-server unit tests pin complete request/response/event JSON round trips, contiguous sequence, untrusted metadata and stable invalid/unknown method errors.
- TUI unit coverage pins stable event projection and direct-event ownership. A production RealCompositionHarness turn proves the actual factory/plugin/service/native backend/local client stream UserInput→TurnStarted→AssistantDelta→Usage→TurnFinished contiguously.
- The first no-fail-fast run found one service publication type error (`Arc<Arc<AppServer>>`) and four stale historical-profile order expectations. Service publication now stores `AppServer` so Context returns `Arc<AppServer>`; migration expectations retain existing routing then insert app-server immediately before TUI. Only failed targets were rerun after those fixes.
- `cargo fmt --all --check` — pass.
- Warnings-denied affected all-target clippy — pass across app-server/config/TUI/CLI.
- Affected inventory — **172 tests green**: app-server 3, config 46, TUI 58 and CLI 65. Current source inventory is **925 tests / 56 consolidated non-empty suites**; the last full workspace milestone remains 848/54.

Dependency impact and next task:

- X05 is active; X06 is newly actionable. X05 extends the stable method catalog with redacted auth/model/MCP/plugin/settings snapshots and mutations so dialogs need no filesystem knowledge. It must reuse owner services and preserve commit-before-publication rather than duplicating their stores.

## 2026-08-28 — X05: registry-owned app-server controls

Tracker: `X05` complete; `X06` active. Rollup: **105 complete · 6 active · 181 not started · 0 blocked**. The active rows are POR04/POR05 live evidence, X06 SDK clients, E11/E13 native platform evidence and QSEC01 owner-approved security policy.

Outcome:

- Added optional default plugin `app-server-controls` after `routing-auth`. The base `app-server` turn host remains independently composable; the control plugin injects settings, credentials, authorization, secret prompt, providers/models, MCP and routing, registers one token-owned control generation and contributes fourteen exact `app_server_method` inventory rows. Context LIFO removes the generation before the base server. Default composition is now 62 plugins and 37 service/seam keys; schema remains 17 because no existing plugin gained a persisted dependency and intentional complete profiles may omit controls.
- Extended protocol v1 and `LocalAppClient` with typed `authorization/{list,start,answer,cancel,logout}`, `providers/{list,select}`, `models/{list,select}`, `mcp/list`, `plugins/list` and `settings/{list,get,replace}`. `initialize` truthfully advertises whether the optional generation is installed. Control notifications use the same serialized, contiguous event plane with no session id.
- Authorization list is credential-blind and returns `inspected:false`; it never calls provider inspection or keychain existence APIs. Start resolves the current routing selection, requires the flow-owned credential reference to match that provider profile, attaches a validated non-zero operation correlation and forwards only its masked prompt metadata. Answer/cancel use the broker prompt id. Receipt publication follows AuthorizationService's credential commit, validation record and authoritative readback, and contains no value.
- Added an effect-owned prompt subscription plus a pending-prompt drop guard. Cancellation or future drop removes its broker row and emits safe `answered:false`, so an abandoned client cannot submit a late secret. App-server lifecycle cancellation cancels and awaits an in-flight control future rather than detaching it.
- Provider/model methods reuse `RoutingService` durable Settings CAS. Catalog success retains exact freshness/revision/capabilities. Failure visibly returns both an already-effective unproven id and the provider-owned default; only the default is selectable without evidence. This fixed the production fake-provider case without weakening model admission.
- MCP serialization reuses the registry's redacted immutable snapshot. Plugin inspection reuses the live shared exact inventory, including the control method owners. Neither plane reconstructs private definitions or paths from files.
- `SettingsSchema` is wire-dark by default. Production non-secret namespaces (`credentials` references, `routing`, `native-tools`, `web`) explicitly call `with_wire_exposure`. Unattested rows expose only namespace/revision/timing and reject mutation; exposed replacement calls the existing expected-revision durable writer and synchronous owner watchers before returning. S15 remains open for field/path redaction and managed third-party policy.

TDD, diagnosis and verification:

- Four new contracts cover fail-closed settings exposure, broker-future drop revocation, correlated masked authorization through committed secret-free receipt, and a production-composed client exercising every control family with bounded operation settlement.
- The first combined run stopped before tests on one formatter line and then one ownership compile error; each received a code change before resumption. The no-fail-fast test run then exposed a real 60-second hang in `authorization/list`: `CredentialsService::describe` reached the native keychain's password-backed existence probe on this locked host. The method was changed to explicit non-inspecting status and the exact product test settled in 140 ms; no credential value was printed or returned.
- The same exact test then exposed two model-evidence errors in sequence: fallback omitted the currently effective id, and selecting that unproven id correctly failed. Fallback now retains it as visible/unselectable and identifies the provider default as selectable; the test selects the default and persists the resulting route.
- `cargo fmt --all --check` — pass.
- Warnings-denied affected all-target clippy — pass across core/settings/credentials/authorization/API-key/native-tools/web/routing/app-server/TUI/config/CLI.
- All **193 affected tests** are green using the first no-fail-fast results plus only the diagnosed failed/exact rechecks: app-server 4, authorization 3, API-key 6, config 46, core 38, credentials 6, native-tools 6, routing 3, settings 9, web 19 and CLI 53. Current source inventory is **929 tests / 56 consolidated non-empty suites**. The last full workspace milestone remains 848/54; no new full-gate, live-provider, real-keychain or socket/SDK claim is made.

Dependency impact and next task:

- X06 is active. It must package the typed Rust client and a TypeScript protocol client/example that both prove initialize, start/resume, streamed events, cancellation and the X05 controls without inventing a second protocol or exposing the in-process server over an unauthenticated network endpoint.

## 2026-08-28 — X06: fixture-locked Rust and TypeScript SDK clients

Tracker: `X06` complete; `R04` active. Rollup: **106 complete · 6 active · 180 not started · 0 blocked**. X07 remains dependency-blocked by E08; R04 is the next credential-blind delegated-runtime vertical.

Primary toolchain evidence:

- Official TypeScript setup guidance recommends a project-local npm compiler plus lockfile. The registry reported exact current `typescript` 7.0.2, which is pinned rather than ranged.
- Official Node documentation reports built-in erasable TypeScript stripping stable from Node 24.12/25.2 and requires explicit `type` imports/file extensions. The package therefore declares Node >=24.12 for its runnable source example; published consumers use compiled ESM/declarations.

Outcome:

- Added 42nd crate `heycode-sdk`, depending only on core plus serde/async utilities. It is the sole Rust owner of app-server protocol-v1 public wire types, body-free errors, method constants, raw `AppTransport` and generic cloneable `AppClient<T>`. The SDK imports no Agent/runtime/settings/credential/MCP/server code.
- Refactored `heycode-app-server` to consume and re-export those types. It implements `AppTransport` directly for local `AppServer`; `LocalAppClient` is now the generic SDK client over that implementation. Internal typed notifications serialize to raw JSON, then the SDK parses them, so TUI and production tests exercise the external client boundary without a second local client implementation or spawned bridge task.
- Each exchange owns one raw request, zero or more notifications and one response under the caller token. The client enforces 4-MiB frames, checked non-wrapping ids, exact JSON-RPC response correlation, closed serde events, opened-session identity and contiguous per-operation notifications. The first sequence may be arbitrary because the host counter is global. Closed notification receivers disable their select branch, preventing a perpetually ready `None` from starving a completed response.
- Added typed `start`, `resume(expected_session_id)`, `open`, streamed `turn`, concurrent `cancel`, `close` and every X05 control method. Host composition remains the only authority choosing new versus resumed session/path; resume proves the returned identity instead of pretending the client can select host storage. Streaming uses a caller-owned bounded channel, and examples own/await both turn and cancel futures.
- Added `sdks/typescript` package `@heycode/sdk` with TypeScript 7.0.2 exact lock, Node >=24.12, strict ESM/declarations and no runtime dependency. The raw transport API forbids frame logging. Runtime decoders validate the JSON-RPC envelope, every closed event, authorization/route/model/plugin/settings response, safe integers, UTF-8 byte cap, 128-level JSON depth, session identity and sequence; compile-time casts are not used as trust.
- Added shared `sdks/fixtures/app-server-v1.json`, parsed by both languages, covering initialize/session/turn, all twelve event variants, credential-blind auth and exposed settings casing. Added runnable Rust and TypeScript examples for start, exact-id resume, typed streaming, concurrent cancel and cancelled settlement. No stdio/socket listener or IDE authentication is claimed.

TDD and verification:

- The Rust tests were written against the empty crate and first failed on every missing SDK symbol. Implementation then made the typed lifecycle and sequence-gap tests pass; the shared fixture added a third consolidated test.
- `npm test` runs strict TypeScript build, example typecheck, three Node tests and the runnable `.ts` example — pass. The tests cover typed lifecycle/control use, sequence-gap refusal and every shared fixture event. npm audit reported zero vulnerabilities for the three-package compiler graph.
- `cargo fmt --all --check` — pass.
- Warnings-denied affected all-target clippy — pass across SDK/app-server/TUI/CLI.
- Rust SDK 3 + app-server 4 + two exact production CLI app-server tests = **9 affected Rust tests**, all pass. `cargo run -q -p heycode-sdk --example typed_client` and the TypeScript example pass. Current source inventory is **932 tests / 57 consolidated non-empty Rust suites**, plus 3 TypeScript tests. The last full workspace milestone remains 848/54; no full-gate/provider/external-endpoint claim is made.

Dependency impact and next task:

- R04 is active. It must extend the pinned Codex 0.146.0 app-server boundary with credential-blind ChatGPT account state, model catalog and exact capabilities using official methods and safe installed-CLI verification, never reading auth files/tokens.

## 2026-08-28 — R04: pinned Codex account/model/capability discovery

Tracker: `R04` complete; `R06` active. Rollup: **107 complete · 6 active · 179 not started · 0 blocked**. R04 plus C07 unlocks R06 primary Codex sessions, moving that row out of the dependency-blocked bucket.

Primary evidence:

- Current [official OpenAI Codex app-server documentation](https://learn.chatgpt.com/docs/app-server) defines stable `account/read`, `model/list` and `modelProvider/capabilities/read`. Account inspection supports `refreshToken:false`; model rows expose effort options/default, visibility/default, upgrade and input modalities.
- Generated the exact installed `codex-cli 0.146.0` schema under a temporary empty `CODEX_HOME`. Its aggregate hash remains `1a9a00c1ee35d44c8e04e92b394263544f90acaa7afe3c7023b08d9f0eb0d161`. Pinned response hashes: account `a8b27a203541460d6593723f646900e1df4417e0de43dfc36c485643810e1f1a`, model list `6e5e52922a2cd66123b074ac0ef197557a151120db815b3a6ca8402da5aec7a0`, provider capabilities `e5e93e7d50e0f7c2c640ffe4d53107b97e36943680504184932fcfab4c132e71`.
- The exact schema differs from current examples in reviewed details such as Bedrock's `usesCodexManagedCredentials`; the pinned schema, not a mutable web example, owns 0.146.0 parsing.

Outcome:

- `runtime-codex` now advertises `models=Supported`; every session/control capability remains Unsupported. Plugin registration remains side-effect-free and launches nothing.
- `account()` re-resolves/re-hashes/re-versions the installed runtime, initializes a fresh contained app-server, sends only `account/read {refreshToken:false}`, parses the pinned API-key/ChatGPT/Bedrock union and closes quiescently. ChatGPT email is bounded/validated then discarded; labels contain only static auth source or closed plan id. Added exact `AccountStatus::NotRequired` for null-account providers that can run without OpenAI auth, distinct from Disconnected.
- `models()` reads exact `webSearch`, `imageGeneration` and `namespaceTools`, then pages `model/list` with `includeHidden:false`, limit 100, 64-page/4,096-total bounds, non-repeating cursors and global id/alias uniqueness. Hidden rows do not publish. Effort ids/defaults/descriptions, modalities, default flag and upgrade ids are validated.
- Normalization stays conservative: explicit effort list and image modality map reasoning/image support; webSearch maps true/false; namespaceTools true proves tool support but false remains Unknown; imageGeneration is validated but never mislabeled as input; upgrade creates Deprecated replacement evidence; unknown limits/lifecycle remain Unknown. Catalog rows sort deterministically under provider `codex` / `DelegatedAgent`.
- Each account/catalog success, remote/protocol error or caller cancellation closes the fresh client using an uncancelled teardown token. Original error class wins only after reader/input/contained process settlement. Codex errors map to fixed RuntimeError classes with no body/account/model data.

TDD and verification:

- Tests were added first and failed at the missing NotRequired state/unsupported descriptor. Five new cases cover connected ChatGPT plan without email retention, signed-out versus auth-not-required, exact two-page capability/catalog mapping, malformed plan/repeated cursor refusal and cancellation settling the descendant tree before return.
- One older registration test correctly failed because models changed from Unsupported to Supported. It now proves registration performs no eager discovery and all session controls remain Unsupported.
- `cargo fmt --all --check` — pass.
- `cargo clippy -p heycode-runtime -p heycode-runtime-codex --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-runtime -p heycode-runtime-codex --no-fail-fast` — **50 passed**, 0 failed (runtime 15; Codex 35).
- `HEYCODE_CODEX_ACCOUNT_E2E=1 ...configured_local_codex_0146_account_and_models... --exact` — pass in 1.69 seconds against the installed official runtime's normal account store. The canary prints no label, token, auth file, response body or raw frame and invokes no thread/turn/model inference.
- Current source inventory is **937 tests / 57 consolidated Rust suites**; the last full workspace milestone remains 848/54.

Dependency impact and next task:

- R06 is active. It must reuse the same pinned connection/normalizer/session query foundations to implement primary start/resume/fork/steer/compact, normalized event streaming, permission/question callbacks and quiescent cancellation without copying Codex tokens or rollout history.

## 2026-08-28 — R06: Codex primary-runtime session bridge

Tracker: `R06` complete; `R09` active. Rollup: **108 complete · 6 active · 178 not started · 0 blocked**. Recalculated pending buckets: 59 actionable implementation · 4 actionable verification/docs · 112 dependency-blocked · 3 dependency-ready optional.

Outcome:

- `runtime-codex` advertises `resume`, `fork`, `steer`, `permissions`, `questions` and `compaction` as Supported. `start`/`resume`/`fork` map to `thread/start`/`thread/resume`/`thread/fork`; each session owns one connection, one driver task and one lifecycle child token for its entire life, and one async operation gate serializes `turn/start` against `thread/compact/start`. `turn/steer` always carries the required `expectedTurnId` precondition.
- Durable v2-only `runtime/linked` binds a heycode session to its provider-native thread and is written before any bus/UI publication. Routing, the app-server native backend, TUI permission/question surfaces and CLI admission for delegated interactive sessions all land in the same slice.
- `follow_up` and `RuntimeInput` attachments remain explicitly Unsupported. The pinned protocol has no queued post-turn delivery method; `thread/inject_items` appends raw Responses items straight into model-visible history, which is a different contract, and the implemented `UserInput` mapping is text-only.

Defect found and fixed during end-to-end acceptance inspection:

- The handoff reported R06 as complete pending one enum-audit fixture. Inspecting acceptance rather than trusting that report, the pinned `codex-cli 0.146.0` schema was regenerated locally (`codex app-server generate-json-schema --out <tmp>` under an empty `CODEX_HOME`) and its `ServerNotification` union compared against the dispatcher. The union has **70** methods; 14 were handled and 31 ignored, leaving **26 falling through to `_ => Err(protocol())`**. Because a driver-task error is terminal, any one of them killed the whole primary session.
- At least three are reachable in ordinary use: `thread/name/updated` (Codex auto-names a thread right after its first turn, so most real sessions would have died), `mcpServer/startupStatus/updated` (any user with MCP servers in their Codex config) and `skills/changed` (local skill-file edits).
- Fix: separate version drift from unmodelled-but-pinned events. Only a method outside the pinned union is a protocol error; the 56 pinned methods carrying no state this session owns are an explicit listed ignore.
- Server→client requests keep the opposite default. Exactly four pinned human-interaction requests are answered — `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval` and `item/tool/requestUserInput`. Legacy `applyPatchApproval`/`execCommandApproval`, `mcpServer/elicitation/request`, `item/tool/call`, `attestation/generate` and `account/chatgptAuthTokens/refresh` fail the session loudly. Servicing a token refresh would end credential-blindness, and silence would hang the runtime, so the connection terminates instead.

TDD and verification:

- Red first: the primary-session fixture was changed to emit `thread/name/updated`, `mcpServer/startupStatus/updated` and `skills/changed` mid-turn, exactly as the real runtime does. The existing acceptance test failed at the first event read with `RuntimeError { code: Protocol }`.
- Green: completed the pinned ignore list to 56 entries and added two audit tests. `pinned_notification_union_is_closed_and_dispatched` asserts handled ∪ ignored equals the pinned union exactly and reports both differences by name; `every_handled_notification_has_a_dispatch_arm` is a source-law check over `include_str!("session.rs")` proving the lists cannot drift from the `match`.
- The missing `SessionEventKind::RuntimeLinked` / `"runtime/linked"` case was added to `kind_tags_match_wire_names`, restoring the 23-variant enum audit.
- `PINNED_HANDLED_NOTIFICATIONS` lives inside the test module: a const consumed only by tests is production dead code and fails `-D warnings`.
- `cargo fmt --all --check` — pass.
- `cargo clippy -p heycode-runtime-codex -p heycode-session --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-runtime-codex -p heycode-session --no-fail-fast` — **128 passed**, 0 failed (Codex 15 unit + 25 integration; session 24 unit + 64 integration).
- Current source inventory is **941 tests / 57 consolidated Rust suites**; the last full workspace milestone remains 848/54. No full-gate, live-provider or cross-platform claim is made for this slice.

Dependency impact and next task:

- R06 has no dependent rows, so it unlocks nothing new; the actionable implementation bucket stands at 59.
- R09 is active. It must give Claude the same primary-session surface over the official Claude Code streaming protocol, reusing the R02 normalized event boundary, the R06 lifecycle shape and the same closed-union discipline against the installed CLI's actual programmatic surface.

## 2026-08-28 — A03: durable inbox follow-up/steer/inject

Tracker: `A03` complete. Rollup: **109 complete · 6 active · 177 not started · 0 blocked**. Buckets: 60 actionable implementation · 4 actionable verification/docs · 109 dependency-blocked · 4 dependency-ready optional.

Outcome:

- `Agent::submit_inbox`, `cancel_inbox`, `pending_inbox` and `send_follow_up_cancellable` put the C06 durable vocabulary to work. Three axes stay independent: target queue (`FollowUp`→next turn, `Steer`/`Inject`→next step), idle wake (follow-up and steer wake, inject never does) and model visibility (only a claim).
- Busy/idle is read from `AgentCancellation::is_turn_active()` — the same lease that owns turn cancellation — rather than a duplicated flag or UI state.
- The turn drains next-step work at the top of every step **and again immediately before settling**, running another step when anything was claimed. That second drain is what makes "busy never wakes" safe rather than a message-loss bug: input arriving after the last boundary would otherwise be claimed by nobody.
- A follow-up queued while busy is not started by the agent itself, which would race the caller's turn ownership. Settlement publishes pending counts with an explicit owed wake after the durable close, so exactly one owner starts exactly one turn. `UiEvent::InboxUpdated` carries that; A05 wires the TUI.
- `Session::append_inbox_claim` appends the removal splice and its `user/message` atomically, one message per pair. `append_kinds_atomically` previously skipped the inbox validate/commit path entirely — safe only because both callers were attachment pairs — and now threads the stateful projection through the batch, committing it only after the whole batch is durable.
- The turn body was refactored to `run_turn(TurnOpening, …)`; `Fresh` admits caller text plus attachments, `FollowUp` claims the oldest queued input under the already-held turn gate. A refused empty follow-up releases its lease and writes nothing.

TDD and verification:

- Four contract tests define the slice: idle wake rules with inject explicitly not waking plus idempotent cancellation; mid-turn arrival proving a busy submission returns `Queued`, enters the running turn through the pre-settlement re-drain, and is model-visible in the next request; atomic claim ordering (`agent/inbox/splice` → `user/message` → `turn/start`) with exactly one message opening a follow-up turn; and an empty follow-up refused without touching the log, requests or the turn lease.
- The mid-turn provider runs a deterministic hook from inside its stream and holds only a `Weak<Agent>`, so the registry-owned provider cannot keep the Agent alive.
- `cargo fmt --all --check` — pass.
- `cargo clippy -p heycode-agent -p heycode-session --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-agent -p heycode-session --no-fail-fast` — **156 passed**, 0 failed (agent 10 unit + 58 integration; session 24 unit + 64 integration).
- Source inventory **945 tests / 57 suites**; last full workspace milestone remains 848/54.

Dependency impact and next task:

- A03 unlocks `A05` (TUI steering/follow-up wiring, still gated on U18), `O04` background jobs, `O10` goals and `O13` schedules. `O04` rises to 8 transitive descendants.
- Next highest-unlock ready rows: `P11` (11), `O01` (9), `MCP03` (8), `O04` (8).

## 2026-08-29 — O01: subagent provider and continuation contract

Tracker: `O01` complete. Rollup: **110 complete · 6 active · 176 not started · 0 blocked**. Buckets: 62 actionable implementation · 4 actionable verification/docs · 106 dependency-blocked · 4 dependency-ready optional.

Outcome:

- New `heycode-agent::subagent_provider` owns the contract: validated `SubagentProviderId`/`SubagentId` newtypes, the two independent axes `SubagentSeed{Fresh,ForkParent}` and `SubagentContinuation{OneShot,Continuable}`, tri-state `SubagentCapabilities{fork,continuation,interrupt}`, `SubagentProviderDescriptor`, `SubagentRequest`, classified body-free `SubagentError`, the `SubagentProvider`/`SubagentHandle` traits and the effect-owned `SubagentRegistry`.
- The previous design encoded delegation as one `fork: bool` plus three spawn methods, silently coupling the two axes and making fork+continuable unreachable. They are now orthogonal, and the missing combination is a capability question rather than a hole.
- `Unknown` never counts as support: `descriptor.supports(request)` requires exact `Supported` for a fork seed or a continuable request. Selection is deterministic registration order; duplicate provider ids fail loud rather than shadowing.
- Plugin `subagent` now provides service `"subagents"` and registers `NativeSubagentProvider`, which proves all three capabilities. One context effect drops providers and interrupts every live child. The old runner kept a `HashMap` of continuable children that nothing ever removed, leaking an `Arc<Agent>` and a durable session per child for the process lifetime; the registry now owns that lifetime.
- The four model-facing tools (`task`, `send_message`, `list_tasks`, `interrupt_task`) resolve through the registry and carry the live tool cancellation token into delegation. `send_message` previously scoped `TASK_DEPTH` to a literal `1`, letting a nested continuable chain reset the nesting count; the handle now records `request.depth + 1` at creation. O03 still owns the complete ownership/authority model.
- `SERVICE_SUBAGENTS` was added to `BUILTIN_SERVICE_KEYS`, the AGENTS §3 table and both exact composition audits. No new plugin was introduced, so no config schema migration was required.

TDD and verification:

- The evidence for "migrate without behavior loss" is that **all eight pre-existing subagent tests pass unedited**. Rewriting them to match the new code would have destroyed exactly that evidence.
- Four new integration contracts pin the new surface: evidence-driven selection with unproven combinations refused as `Unsupported` and the provider never invoked; duplicate registration failing loud with the first row retained; the composed world publishing exactly one native provider proving all three capabilities; and context shutdown leaving no provider rows and no live children. Three unit tests pin `Unknown`-is-not-support, boundary validation of ids/labels/prompts, and bounded single-line errors.
- Both exact composition audits failed first, which is them working as designed for a new service.
- `cargo fmt -p heycode-agent -p heycode-cli -p heycode-tui -p heycode-session -p heycode-runtime-codex -- --check` — pass. (The workspace-wide check was deliberately not run: a delegated lane is mid-edit in `heycode-llm`.)
- `cargo clippy -p heycode-agent -p heycode-cli -p heycode-tui --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-agent -p heycode-cli -p heycode-tui --no-fail-fast` — **200 passed**, 0 failed.

Dependency impact and next task:

- O01 unlocks `O02` (fresh/fork/continuable native providers), `O03` (ownership/depth/authority), `O06` (worktree provider), `O09`, `O14` and is a prerequisite for the `R05`/`R08` delegated subagents. Dependency-blocked drops from 109 to 106.

## 2026-08-29 — R09: Claude primary-runtime session bridge

Tracker: `R09` complete. Rollup: **111 complete · 5 active · 176 not started · 0 blocked**. Buckets unchanged at 62 · 4 · 106 · 4 — R09 has no dependent rows.

Primary evidence:

- Installed `claude 2.1.250`. Every probe ran under an isolated `CLAUDE_CONFIG_DIR`/`HOME`; help and version output only, no model turn, no auth file read.
- Published documentation never prints the control-protocol wire literals, so they were closed from the shipped implementation itself. `strings` over the installed binary gives the exact envelope types `control_request`, `control_response`, `control_cancel_request`, `control_request_progress` and 95 `subtype` literals including `interrupt`, `can_use_tool`, `request_user_dialog`, `set_model` and `set_permission_mode`, plus the `behavior:"allow"|"deny"` permission vocabulary. Reading the shipped bundle is credential-blind, offline and exact.

Outcome:

- `runtime-claude` advertises `resume`, `fork`, `steer`, `follow_up`, `permissions`, `questions` and `compaction` as Supported; `models` stays Unsupported because runtime-native discovery is a separate concern.
- One session owns one `--print --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-mode default` process, its driver task and one lifecycle child token for the session's whole life. A permission mode that decided on heycode's behalf would hide operator decisions, so the session always asks.
- `--session-id` lets the host choose identity, so `system/init` is *verified* against an expected value rather than trusted; fork deliberately reports a new id and is not constrained. The CLI has no synchronous turn ack, so the host-minted message uuid is the turn id — it returns as `user_message_uuid` and names the turn in an interrupt receipt.
- `shouldQuery:false` is the documented append-without-turn primitive, which is why Claude supports `follow_up` where Codex cannot. Compaction is the `/compact` command, and only an observed `system/compact_boundary` counts as evidence that it happened. Cancel is `interrupt` with `cancel_queued`.
- The stdout frame boundary **inverts** R06's closed-union rule, deliberately: Claude's `SDKMessage` union is explicitly open, so an unmodelled top-level `type` is ignored while envelope shape, session identity, correlation ids, control subtypes and bounds stay strict. Erroring on the open set would kill a healthy session on the first task notification or hook event.
- Only `can_use_tool` and `request_user_dialog` are answered. Every other CLI-originated control request — notably `oauth_token_refresh` and `host_auth_token_refresh` — fails the session loudly, because answering would end credential-blindness and silence would hang the runtime.
- Both allow decisions send a bare `{behavior:"allow"}`. Claude's `updatedPermissions` path carries a `localSettings` destination that writes a persistent rule into the project's settings file and outlives the session; that is strictly broader authority than heycode's `AllowSession` means, so it is never sent.

Defects found while implementing:

- `parent_tool_use_id` was parsed but unused. A non-null value means the frame came from a delegated subagent whose `tool_use` ids belong to a nested context; emitting them would have correlated a nested call against this session's results. The dead-code lint surfaced it, and the acceptance test now asserts a nested frame produces no tool call.
- Complete assistant `text`/`thinking` blocks duplicated the streamed deltas and the `result.result` final text, so `AssistantBlock` now retains only `ToolUse`.
- `spawn_interactive` requires `ProcessSpec::with_interactive_stdio()`; omitting it surfaced as a bare `InvalidRequest` far from the cause.

TDD and verification:

- Six frame-boundary unit tests pin: unmodelled pinned messages ignored rather than fatal; strict envelope/identity/oversize/control-subtype failures; assistant frames projecting only tool calls and flagging subagent origin; text-vs-thinking delta split; replayed user frames ignored while tool results correlate; and compaction trigger validation plus cache-folding usage. Three session unit tests pin v4 UUID generation, the exact identity flags per mode, and that advertised capabilities match implemented controls.
- One end-to-end acceptance test drives a fixture CLI that honors `--session-id`/`--resume`/`--fork-session` exactly as the real one does: hook and plugin frames before init, partial thinking and text deltas, a `can_use_tool` prompt answered once and refused on a second attempt, a `request_user_dialog` question answered, a nested subagent frame that must not emit a tool call, correlated tool call/result, cache-folded usage, settlement, then `/compact` proven by a `compact_boundary`. It also asserts the stdin log contains `shouldQuery:false` and `behavior:"allow"` but never `updatedPermissions`. A second test pins resume/fork identity flags.
- `cargo fmt -p heycode-runtime-claude -- --check` — pass.
- `cargo clippy -p heycode-runtime-claude --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-runtime-claude --no-fail-fast` — **28 passed**, 0 failed (16 unit, 12 integration).
- No live Claude turn was run: the acceptance evidence is fixture-driven production composition, and the crate's separate installed-CLI probe remains explicitly `HEYCODE_E2E`-gated.

Dependency impact and next task:

- R09 has no dependent rows. `R08` (Claude delegated subagent) still awaits `O05`, and `R05` awaits `O05` too, so `O04` background jobs is now the highest-unlock ready row at 8 transitive descendants.

## 2026-08-29 — CAT06 pricing/performance, plus the heycode-http transport gaps it and MCP03 exposed

Tracker: `CAT06` complete. Rollup: **112 complete · 5 active · 175 not started · 0 blocked**. Buckets: 62 · 4 · 105 · 4.

Primary evidence:

- Public unauthenticated `GET https://openrouter.ai/api/v1/models` (no credential involved, so unaffected by this host's dummy-keychain constraint). 387 models; the published `pricing` keys are `prompt`, `completion`, `input_cache_read`, `input_cache_write`, `input_cache_write_1h`, `internal_reasoning`, `audio`, `audio_output`, `image`, `image_output`, `input_audio_cache`, `web_search`, `overrides`. `z-ai/glm-5.3-flash` publishes `prompt` `0.000000075`, `completion` `0.00000025`, `input_cache_read` `0.000000015` — which also independently corroborates the pinned route id.

Outcome:

- `heycode-llm` gains unit-explicit pricing: `ModelPricing` (a component map), `TokenPrice` (exact pico-units plus currency and unit), `PriceComponent{Input,Output,CachedInput,CacheWrite,Reasoning}`, `PriceCurrency`, `TokenPriceUnit{PerToken,PerMillionTokens}`, and advisory `ModelPerformance`. Both are now required `ModelDescriptor` fields, so all 21 construction sites had to state their evidence rather than inherit a default.
- Absent is representable and distinct from zero: an unpublished component is missing from the map, never a zero a consumer could read as free.
- OpenRouter normalizes its five per-token components exactly and refuses a malformed price for the whole row. Media and search components stay unmapped because they are not per-token. DeepSeek and the Codex app-server catalog publish no price or performance evidence, so both record `unknown()` rather than inventing figures.
- `heycode-catalog-file` gained the explicit wire mapping: prices persist as integer pico-units with their currency and unit, the pricing object is omitted entirely when unknown, and an unrepresented currency, unit or component **fails the read** instead of degrading to "no price". That made the conversion fallible out to the store, which is the correct shape for an explicit mapping.
- `heycode-http` gained three things the MCP03 lane proved were missing, all mine because that crate is outside its scope: a bounded response-header map (64 headers, 4 KiB values, lowercased, non-UTF-8/oversized dropped), an `HttpRequest::delete` method, and — the security fix — `redirect::Policy::none()`. reqwest's default followed redirects and strips only `Authorization` across origins, so an automatic hop would have re-sent `Mcp-Session-Id` and any custom-header provider key to the redirect target.

TDD and verification:

- Three new `heycode-http` contract tests: a cross-origin redirect is returned rather than followed, proven by a two-listener fixture asserting the second origin is never contacted and the canary header never leaves; response headers are case-insensitive, lowercased and bounded with oversized values dropped rather than truncated; and `DELETE` reaches the wire as the exact bodyless method.
- OpenRouter's existing normalization test now asserts exact pico-unit values for the three published components and that the two unpublished ones stay absent rather than free.
- A new `heycode-catalog-file` test round-trips pricing and performance, asserts the on-disk form is integer pico-units with currency and unit, and proves an unknown currency, unit or component fails the load rather than silently reading as "no price".
- `cargo fmt` across all touched crates — pass. `cargo clippy --all-targets -- -D warnings` for `heycode-http`, `heycode-llm`, `heycode-catalog-file`, both provider crates, both runtime crates, `heycode-agent`, `heycode-cli`, `heycode-tui` — pass.
- Tests green: http 19, llm 111+43+5, catalog-file 6, deepseek 15, openrouter 7, codex 40, claude 28, agent 62+13, cli 53+13, tui 49+10.
- No full workspace gate is claimed: `heycode-mcp` is mid-refactor in a delegated lane.

Dependency impact and next task:

- CAT06 unlocks `P12` (rate-limit and cost metadata, now 6 descendants) and five other rows; dependency-blocked drops from 106 to 105.
- `P11` (11 descendants) is the highest-unlock ready row, then `MCP03` and `O04` at 8.

## 2026-08-29 — O04: background job registry, settlement notices and the wake budget

Tracker: `O04` complete. Rollup: **113 complete · 5 active · 174 not started · 0 blocked**. Buckets: 62 · 4 · 103 · 5.

Outcome:

- Plugin `agent` now also publishes effect-owned `Arc<JobRegistry>` under service `"jobs"`, plus model-facing `list_jobs` and `cancel_job`.
- **Settlement is exactly once**, enforced by state rather than convention: `settle` refuses a job that is not `Running`, so a duplicate cannot deliver a second notice or spend a second wake token. Cancel is a different question — idempotent while running, false once settled.
- **A settled job delivers a notice, not a turn.** The notice goes through the A03 durable inbox, so it is replayable and is not model-visible until a claim admits it.
- **Waking is budgeted.** A settlement whose delivery would wake an idle agent spends one token; with none left the notice is demoted to `Inject` — durably delivered, just unable to wake. Turn settlement replenishes the budget, and does so *before* announcing, so a settlement landing in that window can still wake exactly one turn. A job storm therefore costs at most one wake per turn rather than one per job.
- The registry owns every `JoinHandle` and cancellation token. Disposal cancels and aborts synchronously, because a context disposer cannot block; nothing is ever detached.

TDD and verification:

- Five unit contracts: label validation with stable ordered ids; settlement exactly once with the first outcome standing; the wake budget demoting rather than dropping and refilling per turn; a non-waking delivery never spending a token; and cancel/dispose releasing every owned task, including that a disposed registry admits nothing.
- One integration contract over the **composed** world proves the production wiring end to end: the first settlement wakes, the second is demoted to the step queue with `demoted_wakes() == 1`, neither notice is model-visible before a claim, a second settlement of the same job is refused, and a real turn refills the budget so the next settlement wakes again.
- Two composition audits failed first and were correct to: registering the tools without declaring them in `Plugin::inventory()` broke the exact declared-vs-live comparison, and the descriptor needed the `Tool` family. That audit is the attribution guarantee `/plugins verbose` depends on, so the fix was the declaration, not the fixture.
- An `install_jobs_for_test` shim was written and then removed: the composed world already provides the registry, so the test resolves it from the context and thereby proves the production path instead of a stand-in.
- `cargo fmt` — pass. `cargo clippy -p heycode-agent -p heycode-cli -p heycode-tui --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-agent -p heycode-cli -p heycode-tui -p heycode-session --no-fail-fast` — **294 passed**, 0 failed.

Dependency impact and next task:

- O04 unlocks `O12` (workflows) and contributes to `O05`/`O07`; dependency-blocked drops from 105 to 103.
- Delegated lanes in flight: `mcp-impl` (MCP03/MCP06), `pan01-impl` (Anthropic provider), `p11-impl` (token counting registry).

## 2026-08-29 — MCP03/MCP06 (delegated lane, owner-reviewed) and the HttpResponse Debug regression

Tracker: `MCP03` and `MCP06` complete. Rollup: **115 complete · 5 active · 172 not started · 0 blocked**. Buckets: 66 · 4 · 97 · 5.

Corrections to my own instruction, recorded because both were mine:

- I handed the lane the wrong MCP03 acceptance ("reconnect/resume/session-id/security") from the handoff prose. The authoritative row is "Initialize/list/call/cancel fixtures pass"; reconnect is `MCP07`. The lane caught it rather than building to my text.
- I initially let the lane define a parallel transport trait inside `heycode-mcp` because `heycode-http` could not expose response headers. That was the wrong boundary: the correct fix was to extend `heycode-http`, which I own. Six public types and a module were deleted once it landed, and the deterministic protocol doubles now implement `heycode_http::HttpTransport` itself, so scripted cases exercise the production path.

Primary evidence and the era decision:

- The current MCP revision `2026-07-28` **removed** sessions, the server-initiated GET stream and `Last-Event-ID` resume from Streamable HTTP. The tracker's surrounding rows assume session ids, and real servers today speak the handshake era, so the implementation targets `2025-11-25` behind a closed `McpProtocolVersion` enum with the removal recorded (spec URL included) in both the enum's doc comment and the README. The stateless era becomes a new arm, not a rewrite.

Outcome:

- MCP06: a tool generation is atomic. The paginated walk runs while previous rows stay live; end-of-list is an absent or null `nextCursor` and an **empty string is a valid cursor**; a repeated cursor, an invalid row or an exceeded page/tool budget rejects the whole generation without publishing; a `list_changed` epoch observed during the walk discards the candidate. A complete unraced candidate swaps all-or-nothing, and a contested name rolls back and restores the previous rows. The stdio bridge now uses this owner instead of its own registration path.
- MCP03: exact `Accept`/`Content-Type`, runtime JSON-vs-SSE branching, session-id echo plus `MCP-Protocol-Version` on every later request, 404 → re-initialize-without-session → retry once, DELETE termination tolerating 405/404, one cancellation token and per-operation budgets. Credential-reference headers are refused with `Unauthorized` rather than connecting without them; `MCP04` owns resolving them.

Security regression found by the lane and fixed by me:

- Adding the response-header map made `HttpResponse`'s derived `Debug` render header values — and it was already rendering the body. A server controls both, and response headers routinely carry session identity (`Set-Cookie`, `Mcp-Session-Id`). `Debug` is now hand-written and reports status, content type, header **names** and body length only. A regression test asserts three canaries (cookie, session id, body) never appear while names and sizes still do.

Owner verification (not taken on the lane's report):

- Re-ran `cargo clippy -p heycode-mcp --all-targets -- -D warnings` — clean; `cargo test -p heycode-mcp --no-fail-fast` — **52 passed** (8 unit, 44 integration).
- Independently checked the security-critical claims in source: `McpSessionId` renders `McpSessionId([REDACTED])`; `McpHttpError` variants carry only a status code, a JSON-RPC code, or static field/requirement labels — no endpoint, header or body; the absent `Origin` header is documented as a server-side obligation, which is what the spec actually places on servers.
- `heycode-http` is 20 tests. `cargo fmt --all --check` is not claimed: the `p11-impl` lane is mid-TDD in `heycode-llm` with tests referencing types it has not implemented yet.

Documented gap, deliberately not hidden:

- **HTTP MCP servers are not user-configurable.** `[mcp.servers.<name>]` accepts only `command`/`args`/`env`, so the new transport has no producer in the composition root. MCP03's acceptance is met as a transport, but the config/composition work that makes it reachable is unrouted and is now called out in STATUS.md rather than implied complete.

Dependency impact and next task:

- MCP03/MCP06 unlock `MCP04`, `MCP07`–`MCP09`, `MCP11`, `MCP12`, `MCP13`; dependency-blocked drops from 103 to 97 and actionable implementation rises to 66.

## 2026-08-29 — MCP transport selection: giving MCP03 a configuration producer

No tracker row: this closes the gap the previous entry documented, so "Streamable HTTP transport: complete" is not a hollow claim.

Outcome:

- `[mcp.servers.<name>]` selects a transport explicitly: `command` (stdio) or `url` (Streamable HTTP). Naming both is ambiguous, naming neither leaves the server unreachable, and stdio-only `args`/`env` beside a URL would silently discard operator intent — all three fail loud at load and name the offending server rather than defaulting.
- `McpServerCfg::transport` resolves the choice at the composition boundary, so `mcp_plugin` receives a fully resolved `McpServerSpec` instead of deciding a default deeper in.
- A configured HTTP endpoint is registered as a real, inspectable definition. Its connection is **not** activated: MCP04 owns credential-reference resolution, so no tools register for it and nothing is faked. `/mcp` shows the server rather than hiding it.
- No config schema bump: both new fields are optional additions inside an existing section, and no plugin ordering changed.
- `McpServerSpec` deliberately has no `Debug`, which the compiler caught — a stdio server's `env` may carry credentials, which is exactly why `McpServerConfig` has none.

TDD and verification:

- Four config contracts: a stdio server keeps its exact argv and environment; a URL selects Streamable HTTP; an ambiguous or absent transport fails loud naming the server; stdio-only fields beside a URL are refused.
- One composition contract proves an HTTP server reaches the registry as a visible definition while registering no `mcp__remote__*` tool and launching no process.
- `cargo fmt` and `cargo clippy -p heycode-config -p heycode-mcp -p heycode-cli --all-targets -- -D warnings` — pass.
- `cargo test -p heycode-config -p heycode-mcp -p heycode-cli --no-fail-fast` — **169 passed**, 0 failed (config 11+39, mcp 8+45, cli 13+53).

## 2026-08-29 — P11 token counting, PAN01 Anthropic provider, and a green full workspace gate

Tracker: `P11` and `PAN01` complete. Rollup: **117 complete · 5 active · 170 not started · 0 blocked**. Buckets: 68 · 4 · 93 · 5.

**Milestone: the full workspace gate is green** — `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast` at **1079 passed / 0 failed**. The previous full milestone was 848 tests at schema 14; this is schema 18.

Correction to my own instruction, again mine: I gave the PAN01 lane the acceptance "Auth, model list and headers validate live" from an earlier summary. The authoritative row is "**Model list and token count available**". Token counting was therefore missing from the delivered slice, and I implemented it rather than marking the row on the wrong criterion. Three of my delegated instructions have now carried acceptance text from prose rather than the tracker row; reading the row directly is the fix.

Owner verification of both lanes (not taken on report):

- Re-ran both gates. `heycode-llm` 43 + 132, `heycode-provider-anthropic` 2 + 21.
- P11's claim that ties break by id, not registration order, is real: counters live in a `BTreeMap` keyed by id and `sort_by_key` is stable, so id order survives within an evidence rank. A `HashMap` would have made the same code silently non-deterministic.
- PAN01's baked-in facts were the real risk and check out against an authoritative source: `max_input_tokens`, `max_tokens` and `capabilities` are the actual Models API fields (there is no `context_window` field), `claude-opus-5` is a real id, `anthropic-version: 2023-06-01` and `x-api-key` are correct, and Anthropic publishes no pricing endpoint — so `ModelPricing::unknown()` is right rather than transcribing a docs table.

Owner-completed work these lanes left open:

- **P11 was not mounted.** The registry had no service key and no plugin, so nothing in a composed world could reach it. Added `SERVICE_TOKEN_COUNTERS`, `ContributionKind::TokenCounter`, and plugin `token-counters`, which provides the registry and registers the local heuristic explicitly — a visible row rather than a fallback hidden inside lookup.
- **PAN01 had no token counter.** Added `token-count-anthropic`: the provider-measured `POST /v1/messages/count_tokens` counter, declared `TokenEvidence::Exact` and scoped to Anthropic, so it outranks every estimate. It refuses tool-result messages (it cannot reconstruct the structured block) and empty transcripts (the endpoint rejects them and zero would be a lie) as `Unsupported` rather than under-counting.
- Wired both provider plugins plus both counter plugins into `compose_world`, the default profile and every exact composition audit. Schema **v17 → v18** adds `provider-anthropic` and `catalog-anthropic` only to exact profiles that already select Anthropic.

TDD and verification:

- Seven new counter contracts: exact-provider evidence scoped to Anthropic and not to another provider; the exact documented request shape with `anthropic-version`, `x-api-key` and content-type; a missing credential refusing with no request sent; unrepresentable content refused rather than dropped; a rejected or malformed reply failing without the response body reaching a diagnostic (canary-asserted); cancellation settling before any request; and the registry preferring the measurement over the estimate **when the estimator was registered first**, so the win cannot come from ordering.
- Six composition audits failed first and were right to — new services, new plugins, new inventory rows and their sorted positions.

Dependency impact and next task:

- P11 unlocks `C11` (token meter exact/estimate envelope pricing), now the highest-unlock ready row at 10 transitive descendants. Dependency-blocked drops from 96 to 93; actionable implementation rises to 68.

## 2026-08-29 — Lane reports reviewed: one speculative variant struck, provenance closed

Both delegated lanes filed full reports after the fact (their first attempts went to their own transcripts rather than through a message). Both were unusually honest about limits, and two items needed owner action.

Struck on P11's own recommendation:

- `EstimationMethod::ModelVocabulary` had no in-tree producer. The lane added it so the rank ladder would be "real today" and offered to delete it. GOTCHAS #140's position wins: a rank reserved for a counter that does not exist is a speculative abstraction, and adding the variant when a tokenizer counter actually lands is a compiler-enforced break that forces every match to be revisited. Removed; `evidence_ranks_exact_ahead_of_every_estimation_method` now pins the same property against the real ladder, and the doc comment records why no placeholder is reserved.

Limits accepted as stated rather than closed:

- `TokenCount` is an enum a consumer can deliberately flatten to a bare `u64` in four lines. Making it opaque would break C11's need to display both arms. The guarantee is therefore "cannot conflate by accident, cannot fabricate at all" — private fields, no public constructor, and `mint` as the single construction site — not "cannot obtain a number".
- The `Arc::ptr_eq` disposal guard is defence-in-depth and not independently observable. The lane found this by mutation-testing its own test, deleted the test that passed with the guard removed, and replaced it with one that pins something real. A test that cannot fail is worse than no test.
- The heuristic estimator is `ceil(bytes/4)` calibrated to Latin-script English prose. It under-counts dense text by roughly 2x (code, base64, minified JSON), models no per-message framing, and its CJK behaviour is coincidence rather than calibration. The error is predominantly in the **under** direction, which is the dangerous one for context pressure. All of this is in the type's doc comment rather than softened.

Provenance closed on PAN01:

- The lane flagged `claude-opus-5` as its riskiest string — taken from an official docs example response but not confirmed against the live service. Verified independently against an authoritative source: it is the current model id. Its fail-visible design was right regardless, since an absent default rejects the generation loudly.
- Zero-limit rejection stands. The field is documented `number|null`, so `null` encodes "unknown" and a literal `0` is genuinely anomalous; reading the docs example's `0` as a Mintlify placeholder was correct (the same block shows `"first_id": "first_id"`).
- Nothing in `src/` carries a context window, output limit or price. Every limit is read from the response at runtime and pricing is `unknown()`; no docs pricing table was transcribed. The only baked-in model id is the default.
- The `heycode-provider-anthropic` row was added to the AGENTS §2 workspace table, which had been missed.

Deliberately not done, and agreed with both lanes: `--provider anthropic` stays out of the selection allowlists and B09 setup discovery, because `heycode-llm` has the Messages adapter but no `AnthropicProvider` — making it selectable would advertise a route that cannot dispatch.

Gate after these changes: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast` at **1079 passed / 0 failed**.

## 2026-08-29 — C11: token meter exact/estimate envelope pricing

Tracker: `C11` complete (row read verbatim: "Token meter exact/estimate envelope pricing … Prompt/tools/state/attachments included"). Rollup: **118 complete · 5 active · 169 not started · 0 blocked**. Buckets: 68 · 4 · 92 · 5.

Outcome:

- `TokenEnvelope` holds one entry per `EnvelopeContributor` — System, Messages, Tools, ProviderState, Attachments — each with its own `ContributorTokens` evidence (`Exact`, `Estimated(method)`, or `Uncounted(reason)`). Entries stay in stable contributor order and re-measuring one replaces it, so a part of a request can never be double-counted.
- **A total is only as good as its weakest contributor.** Exact plus estimated is `Estimated`, never `Exact`. An uncounted contributor makes the total `AtLeast { counted, uncounted }`, naming exactly what is missing. There is deliberately no plain `u64` total accessor, so a caller must match the variant rather than receive a number that looks complete.
- **Unknown is not zero.** This is the row's whole point: attachments are genuinely unmeasurable without a vision tokenizer (P11's heuristic refuses them outright), so they appear as `Uncounted(Unmeasurable)` rather than being omitted. Omitting them and summing zero is how a context meter reports room that does not exist — and P11's estimator already errs in the under direction, so the two failure modes compound.
- Cost inherits the weakness of the count. An exact published price applied to an estimated token count is `AtLeast`, not `Exact`. An envelope is request *input*, so a model publishing only an output price is `Unpriced` — substituting one price component for another would misreport cost, the same rule CAT06 applies. A per-million price truncates downward, keeping a cost a lower bound rather than inflating it.
- "Nothing to measure" and "could not measure" stay distinct: an empty envelope is `Exact(0)`.

TDD and mutation verification:

- Eight contracts: weakest-contributor totalling; an uncounted contributor producing a lower bound rather than a zero; an uncounted contributor being unreadable as a number; stable ordering with no double-counting; cost carrying the count's weakness; an unpriced model costing unknown including the output-price-is-not-input-price case; per-million pricing truncating down; and an empty envelope being exactly zero.
- Three deliberate mutations, each turning exactly the intended test red and nothing else: summing `Uncounted` as zero broke `an_uncounted_contributor_makes_the_total_a_lower_bound_never_zero`; letting an estimate produce an exact cost broke `cost_carries_the_weakness_of_the_token_count`; falling back from the input price to the output price broke `an_unpriced_model_costs_unknown_not_zero`.
- `cargo fmt -p heycode-llm -- --check`, `cargo clippy -p heycode-llm --all-targets -- -D warnings` — pass. `cargo test -p heycode-llm --no-fail-fast` — **183 passed** (51 unit, 132 integration).

Dependency impact and next task:

- C11 unlocks `C12` (compaction strategy registry/transactions) and `C13` (`/context` contributor projection) — C13 is the direct consumer of the per-contributor breakdown. Dependency-blocked drops to 92.
- Delegated lanes in flight: `s15-impl` (managed settings + secret redaction verifier) and `e07-impl` (persistent terminal/PTY registry).

## 2026-08-29 — C12: compaction strategy registry and the verified transaction

Tracker: `C12` **active**, not complete (row read verbatim: "Compaction strategy registry/transactions … native/portable/prune strategies settle durably"). Rollup: **118 complete · 6 active · 168 not started · 0 blocked**.

Why active rather than complete: the row names three strategies. The registry, the transaction, **portable** and **prune** all ship and are gated. **Native** does not. Native compaction is performed by the provider or delegated runtime — Anthropic's server-side compaction behind `native_compaction`, or a `RuntimeSession::compact()` — and the `Agent` holds neither an adapter compaction path nor a runtime session today. Shipping a "native" strategy that quietly did a portable fold would be exactly the fake-capability the law forbids, so the row stays open with the reason recorded.

Outcome:

- `CompactionRegistry` owns `CompactionStrategy` implementations by validated `CompactionStrategyId`, with `CompactionKind{Native,Portable,Prune}` and duplicate registration failing loud rather than shadowing. An unknown strategy is named, never silently defaulted to another.
- **The registry verifies the transaction rather than trusting it.** Every run is bracketed by a durable-log snapshot: `Applied` must have committed exactly one `compaction/applied`; `Noop` and every failure must have committed nothing. A violation returns `BrokenTransaction` naming the offending strategy.
- Portable and prune share one `fold_boundary` rule — the keep window, its leading `user/message`, and any `user/attachments` immediately before it — so they cut at exactly the same place and only what replaces the folded span differs. The rule was extracted from the existing `compact()` rather than duplicated.
- Prune commits a fixed `PRUNE_MARKER` instead of a summary. The projection re-injects that text as model-visible input, so the model is told history was dropped rather than handed a shorter conversation it cannot account for; an empty summary would have made a prune indistinguishable from a failed summarization.

TDD and mutation verification:

- A scripted strategy makes the transaction check observable: claiming `Applied` while writing nothing, claiming `Noop` while writing one event, and writing two events for one run each produce a distinct `BrokenTransaction`, while an honest applied run passes.
- Both branches of the check were mutation-verified — neutering either the applied rule or the no-op rule turns `the_registry_verifies_the_compaction_transaction_rather_than_trusting_it` red.
- A third contract covers registration, id-ordered descriptors, duplicate refusal, unknown-strategy naming, pre-run cancellation and disposal; a fourth proves prune folds at the portable boundary, records the marker, and no-ops without writing when the keep window covers everything.
- `clippy -D warnings` caught a `MutexGuard` held across an `.await` in the new test — the same deadlock class GOTCHAS #2 describes and the one P11 flagged for the `context_estimate` migration. Guard scoped.
- `cargo fmt -p heycode-agent -- --check`, `cargo clippy -p heycode-agent --all-targets -- -D warnings` — pass. `cargo test -p heycode-agent --no-fail-fast` — **87 passed** (21 unit, 66 integration).

## 2026-08-29 — S15: managed settings and the verified wire-exposure proof

Tracker: `S15` complete. Row built to verbatim: "Managed settings and secret redaction verifier … Unprovably safe secret schema fails wire exposure". Rollup: **119 complete · 6 active · 167 not started · 0 blocked**. Buckets: 68 · 4 · 90 · 5.

Delegated lane outcome (crates/heycode-settings only):

- `with_wire_exposure()` keeps its signature but is now a claim the service **verifies** at every resolution rather than accepts. Exposure is provable only when every projected path is classified: declared secret (redacted), attested public, or surviving both a key-name lexicon and a structural credential-material detector. Anything unclassified fails `UnprovableWireExposure` and publishes nothing.
- `SettingsWireProjection` covers all seven layers, not just resolved, and is the crate's only value-bearing export — the crate has no serde impls at all. Every `Debug` is hand-written and screens even in namespaces that never claimed exposure.
- `SettingsLayer::Managed` is highest precedence; its leaf paths are locks, and a user write naming one is refused rather than persisting a value resolution would ignore.
- 13 mutations, each breaking exactly its named tests, including no-op redaction, a dropped CAS check, verifying only the resolved layer, and verifying only at registration. The lane also caught and redid a mutation run whose results were shifted by a stale rebuild.

Owner-completed work, and the hole it closed:

- **The proof was being bypassed at the boundary that matters.** `heycode-app-server`'s `setting_row` gated on `wire_exposed()` and then rendered the **raw** layers, so S15's redaction never reached a client. `exposed` now derives from `wire_projection().is_some()` and every value field is read off that projection; `settings/replace` is gated identically, since a namespace that cannot render values must not accept writes to them.
- `AppSettingsSnapshot` gained `managed`, `managed_locks` and `redacted_paths`, all optional and skip-serialized so the shared v1 fixture and the TypeScript client stay compatible.
- `SettingsError::ManagedLock` maps to `Conflict`, not `Unavailable`: the write was refused by administrator policy, and reporting a broken service would send the client to the wrong remedy. A new wire error code would have rippled through the TypeScript SDK for no gain.

A security test that was decoration, and its replacement:

- My first regression test scanned every projected layer in the composed product world for credential-shaped markers. It passed — and **also passed with the raw-layer bug restored**, because no namespace in the default world holds credential material (the credentials namespace holds references by design). It could not fail, so it proved nothing. This is GOTCHAS #143 catching me one section after I wrote it.
- The replacement is a unit test that builds a purpose-made namespace with a declared secret path, asserts the raw layer still holds the canary (or the test proves nothing), and asserts no rendered layer does. Mutation-verified: restoring a single raw-layer read turns it red. It also asserts a non-secret sibling still renders, so redaction stays targeted rather than blanking the namespace.

Honest limits carried forward from the lane, recorded rather than closed:

- The material detector is structural only — issuer prefixes, PEM, JWT, AKIA — with no entropy scoring, because a false positive would refuse a whole namespace. A secret in an unrecognized format, under a non-secret-shaped key, with no owner role, in an exposed namespace, is still projected. The proof rests on the key screen plus explicit roles; the detector only ever denies.
- `scrub_message` is best-effort on free text. In-process owner reads and watcher payloads deliberately carry real values.
- **Nothing yet calls `set_managed`.** The managed layer, its precedence and its locks are implemented and tested, but no producer exists, so managed settings are not yet operator-reachable — the same shape of gap as the MCP HTTP config, and it is recorded here rather than implied complete.

Gate: `cargo fmt --all --check`, `cargo clippy -p heycode-app-server -p heycode-sdk -p heycode-cli -p heycode-settings --all-targets -- -D warnings` — pass. Tests green: settings 29, app-server 5, sdk 3, cli 13 + 53.

## 2026-08-29 — E07: persistent terminal/PTY registry and tools

Tracker: `E07` complete. Row built to verbatim: "Persistent terminal/PTY registry and tools … Owner-scoped sessions, resize/read/write/kill work". Rollup: **120 complete · 6 active · 166 not started · 0 blocked**. Buckets: 68 · 4 · 89 · 5.

**Full workspace gate green: 1134 passed / 0 failed**, `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` clean.

Delegated lane outcome (crates/heycode-exec only):

- `TerminalService` under a new `terminal` service, with real PTYs (`processkit` `pty` feature; lockfile unchanged) and `TIOCSWINSZ` on resize. `open` returns only an id, so no consumer handle exists to drop; the session token is a child of the registry token and the disposer is `close()` itself.
- Output drains continuously into a bounded drop-oldest ring, so an unread terminal never blocks its child; discarded bytes surface as `dropped_bytes` rather than silently vanishing.
- Sixteen mutations, fourteen red with exactly the expected tests. **Two found real defects in the lane's own tests**: a shutdown test whose descendant reaping was actually coming from `subprocess-local`'s disposer rather than the registry (fixed with a standalone `close()` test), and a "concurrent opens" test that never overlapped, which exposed a TOCTOU replaced by atomic slot reservation.
- It also fixed a genuine correctness bug found by its suite: `kill` closed stdin first, delivering an implicit EOF that let a well-behaved child exit cleanly with status 0 — turning a documented hard kill into a hidden graceful shutdown.
- Honest and recorded: removing the drain abort+await in `kill` and no-op'ing `Session::drop` leave every test green, because every drain path self-terminates via cancellation; they make settlement deterministic but nothing observes them.

Owner-completed wiring and the tools half of the row:

- `terminal-registry` registered in `compose_world` and the default profile after `subprocess-local`; `SERVICE_TERMINAL` added to `BUILTIN_SERVICE_KEYS`, the AGENTS §3 table and the §2 `heycode-exec` row; all three exact composition audits updated.
- Six model-facing tools in `heycode-tools`: `terminal_open/write/read/resize/kill/list`. Every `ProcessError` maps to fixed text — a terminal's own output is returned through `read`, never smuggled into a diagnostic. Non-UTF-8 output is replaced rather than refused, so a binary burst cannot make a terminal permanently unreadable.
- Gated behind `[tools] terminals_enabled`, default **false**: a terminal holds a live process for the life of the context, so an intentional profile must ask for it. The flag is an optional field, so no config schema bump was needed.
- **Owner scoping is only real if the model cannot choose the owner**, so the tools bind it from the host at composition. `heycode-tools` may not depend on `heycode-session` under the §2 dependency rules, so the owner is currently one per composed world; per-agent scoping additionally needs a per-agent owner, which O03 owns. Recorded rather than implied.

TDD and verification:

- Three tool contracts, two exercising a real PTY: a full open → write → write → read → resize → list → kill round trip that proves persistence (a variable set by one call is still set for the next) and that the child *observes* the resize via `stty size` reporting `40 120`; cross-session isolation where a second owner sees an empty list and every operation on a known-good id is refused **indistinguishably from an unknown one**, so the id is not a capability; and boundary failures staying fixed text with a malformed id and an oversized write rejected before reaching the terminal.
- `TerminalSpec::new` requires `ProcessSpec::with_interactive_stdio()`; omitting it surfaced as a bare `InvalidSpec` far from the cause, the same trap as `spawn_interactive`.

Deliberately not built: liveness beyond `output_ended` (E09 will want `try_wait` on the seam), session persistence across restarts, and scrollback replay/paging (E06). Windows ConPTY exists through processkit but is unverified here; the resize test is `#[cfg(unix)]`.

## 2026-08-29 — P12: rate-limit and cost facts (active — capture is transport-blocked)

Tracker: `P12` **active**, not complete. Row verbatim: "Rate-limit and cost metadata … `/usage` displays provider facts without guessing". Rollup: **120 complete · 7 active · 165 not started · 0 blocked**.

Outcome — the facts layer ships and is gated:

- `RateLimitScope`, `RateLimitWindow`, `RateLimitSnapshot` and their parsing live in `heycode-llm`, but **no provider header spellings do**. Each provider supplies its own `RateLimitHeaders` mapping; only `Retry-After` is read undeclared, because RFC 9110 is the one genuine cross-provider standard. Baking one provider's names into the shared layer would report nothing for every other provider while still looking like it worked.
- The `anthropic-ratelimit-unified-*` names used in the test fixture were read from the shipped Claude Code 2.1.250 binary. The classic per-quota family is **not** present there, so it is not assumed.
- `RequestCost::Reported` and `::Derived` are distinct facts that never compare equal, so a display cannot present arithmetic as a bill. Deriving requires both the input and output price — falling back to the input rate for output tokens understates it — so a partially priced model yields `Unknown`, which has no number rather than a zero that reads as free.
- Malformed-input policy is deliberately the **opposite** of a catalog's: a rate-limit snapshot drops only the field that failed to parse, because it is advisory telemetry and discarding the parsed fields loses real information. A window with nothing parsed is absent, not a zeroed quota that would read as "no allowance left".

Why active, and the blocker:

- Nothing captures a snapshot yet, and it cannot be wired today: **`HttpTransport::sse` yields only `SseEvent`s and exposes no response headers**, so a streaming inference call cannot observe rate-limit headers at all. The buffered path gained a bounded header map for MCP03; the SSE path has the same gap. Closing it changes a return shape shared by every protocol adapter, so it is real work rather than a line change, and marking P12 complete with an unreachable capture path would be exactly the hollow completion this tracker exists to prevent.
- `/usage` itself is `CMD07` (blocked on `U16` and `C12`), so the display is correctly not this row's job — P12 owes it non-guessed facts, which is what shipped.

TDD and mutation verification:

- Eight contracts: a provider declaring nothing reports nothing; declared headers parsing case-insensitively in both the RFC 3339 and delta-seconds reset spellings; one unparseable header dropping only that field; `Retry-After` read undeclared; an unpublished window being absent rather than an empty quota; a derived cost never presented as reported; a partially priced model yielding `Unknown`; and per-million derivation truncating down.
- Two mutations, each turning exactly the intended test red: falling back to the input price for output tokens broke `a_partially_priced_model_yields_unknown_rather_than_a_half_cost`; keeping an all-absent window broke `a_window_with_nothing_parsed_is_absent_rather_than_an_empty_quota`.
- `cargo fmt -p heycode-llm -- --check`, `cargo clippy -p heycode-llm --all-targets -- -D warnings` — pass. `cargo test -p heycode-llm --no-fail-fast` — **191 passed** (59 unit, 132 integration).

## 2026-08-29 — P12 completed: the SSE header gap closed

Tracker: `P12` complete. Rollup: **121 complete · 6 active · 165 not started · 0 blocked**. Buckets: 68 · 4 · 88 · 5.

The blocker recorded in the previous entry is closed, so the row no longer rests on an unreachable capture path.

- `HttpTransport::sse_exchange` surfaces bounded response headers for a streaming call. Twenty-three transports implement that trait across crates two other lanes were actively editing, so this is a **defaulted** method rather than a signature change: zero implementations broke, and only `ReqwestHttpTransport` — which genuinely observes the response — overrides it.
- The default returns an `unavailable` slot, never an empty map. A transport that cannot report headers says **unknown**; an empty map would read as "the response had no headers", a different and false claim. `SseResponseHeaders` is a fill-once slot because the headers arrive after the stream is constructed, and `None` covers both "not yet" and "never will", which are the same observable state.
- Headers publish for every response that arrived, success or not: a 429's rate-limit headers are exactly the ones a caller most needs.
- `Provider::rate_limit_headers` is the declaration point, defaulting to none. **No in-tree provider overrides it**, because no spelling except Anthropic's `anthropic-ratelimit-unified-*` family could be verified from a primary source, and that family belongs to a provider crate with no streaming inference path. A row whose acceptance is "without guessing" has to apply that standard to itself: an empty declaration is evidence-shaped; a plausible-looking guess is not.

TDD: three new contracts — an SSE exchange observing headers on both a 200 and a 429 with the slot empty until the response arrives; a transport that cannot report headers saying unknown rather than empty; and a provider that declared nothing reporting nothing even when a header it did not claim is present in the response.

Gate: `cargo fmt`, `cargo clippy -p heycode-llm -p heycode-http --all-targets -- -D warnings` — pass. `cargo test -p heycode-llm -p heycode-http --no-fail-fast` — **214 passed** (http 22, llm 60 + 132).

What `/usage` still needs is `CMD07`'s own work plus a provider whose header names can be verified; neither is P12's to invent.

## 2026-08-29 — MCP07: bounded reconnect supervisor

Tracker: `MCP07` complete. Row verbatim: "Bounded reconnect supervisor … Crash-loop exhausts, recovery swaps once". Rollup: **122 complete · 6 active · 164 not started · 0 blocked**. Buckets: 67 · 4 · 88 · 5.

Delegated lane outcome (crates/heycode-mcp only):

- The supervisor decides *whether and when*; the transport owns its own respawn and handshake behind `McpReconnect`. The schedule lives on `McpReconnectPolicy::delay_before_attempt` as definition-owned data — workspace default 500 ms/30 s/10 gives 0, .5, 1, 2, 4, 8, 16, 30, 30, 30 s: ten connects and ≈91.5 s to exhaust. No jitter, deliberately: one supervisor per server means no herd, and determinism lets the backoff be asserted in virtual time against exact numbers.
- **Exhaustion is terminal.** `recover()` answers `Exhausted` thereafter, because an outer Consumer that retries on failure would otherwise rebuild the loop the budget exists to bound. Exhaustion **retires the rows before** reporting `Remove`, so a snapshot never claims no retained generation while its tools are still model-visible.
- Swaps-once is enforced three independent ways, each with its own test: a failed connect never reaches the generation owner (previous rows stay whole all outage — no zero window, no partial set); the episode breaks on first success so exactly one refresh runs, asserted 1→2 with an overlapping tool name so a register-before-release would conflict; and admission is single-flight (5 concurrent calls → 1 Started, 4 AlreadyRunning, 1 generation). The swap itself reuses MCP06's all-or-nothing refresh rather than re-implementing it.
- Red first: a deliberately naive supervisor (no bound, no backoff, no single-flight, no-op disposer, no cancellation check) produced 7 real behavioral failures before implementation.
- **Fifteen mutations; two survived, and both were defects in the lane's own tests.** One: `shutdown()` aborting the task without cancelling the token it threads *into the transport* — nothing observed the outward token, so a transport waiting on it could have leaked a child. Two: mid-attempt cancellation publishing terminal `Exhausted` — that guard arm was never reached because every test cancelled during a backoff, never during a connect. A third weak test passed with a no-op disposer because the task self-terminated. All three were fixed, then all fifteen mutations went red.

Owner verification and one flake I fixed:

- Independently confirmed in source: exhaustion is terminal (`Phase::Exhausted → Exhausted`), `retire()` precedes `report_failure(..., Remove)`, and admission is single-flight.
- The lane flagged that it added tokio's `test-util` feature to its dev-dependencies and asked me to confirm against a full workspace run. I did. Workspace fmt and clippy are clean, and the one failure was **not** feature-related: `heycode-runtime-claude`'s malformed-output test asserted the exact error class `Protocol`, and under full-workspace parallelism the fixture process exceeded the handshake timeout and legitimately reported `Unavailable`. That is a true statement about a loaded machine, not about the parser.
- Fixed properly rather than by loosening: the classification now has a deterministic unit test over `parse_handshake_output`, and the process-driven integration test keeps only what must hold on every path, fast or slow — that no provider body escapes.

Deliberately unmounted, and recorded rather than implied:

- **The supervisor is reachable but never runs.** Both in-crate definition producers build a disabled `McpReconnectPolicy`, and no liveness signal calls `recover()`. The lane declined to flip either, on the grounds that advertising a reconnect budget the crate cannot honor is worse than not advertising one — and that deciding a `Transport` error on one tool call means the connection is dead is a policy question above this row. `MCP10` owns the config surface and the liveness signal.

## 2026-08-29 — A02: pre-step / request / request-error seams, and a deliberate law amendment

Tracker: `A02` complete. Row verbatim: "Add pre-step/request/request-error seams … Current compaction/context consumers migrate". Rollup: **123 complete · 6 active · 163 not started · 0 blocked**. Buckets: 68 · 4 · 86 · 5.

**Law amended, not bent.** AGENTS §3 said a seam is "stored as a service". These three seams' decisions belong to one turn loop, and making them global services would force a subagent child to inherit its parent's step budget — or push every layer into keying state by agent identity. §3 now states the actual rule: a globally-scoped decision is a service (`seam/pre_tool` is global because one tool registry and one approval policy govern every caller); an owner-scoped decision is owned by that owner and reached through the owner's service. Plugin extensibility is unaffected — a plugin resolves `ctx.get::<Agent>(SERVICE_AGENT)` and calls `agent.pre_step_seam().push_shared(layer)`. The lane raised this as a deviation and asked rather than deciding unilaterally, which is exactly right; AGENTS.md itself instructs amending the rule with rationale rather than fighting it.

Outcome:

- Three seams, each at exactly one decision point. `PreStepDecision` decides **before** `step/start` is appended, so a stopped step never appears in the log at all while the turn still closes `turn/end error`. `RequestDecision` allows in-place edits on the delegating path; its `Rebuild` verdict short-circuits because the layer changed durable history — the agent rebuilds from the log exactly once and dispatches that **without re-running the seam**, since later layers must not inspect a request whose log no longer exists. `RequestErrorDecision` has no verdict enum: the decision is the message.
- Seven hand-rolled `emit(Error) + close_turn + return Err` blocks collapsed into a single `fail_request`, which is what makes a request-error seam possible at all — a seam needs THE decision point, and seven of them is not one. The seam is deliberately not cancellable: the closure must finish even on a cancelled turn, or a cancelled failure leaves the turn open.
- Migration is real, not additive: the inline auto-compaction block is **gone** from the step loop and is now `AutoCompactionLayer`, mounted only when `compaction.auto`. `CompactionHandles`/`compact_with` let the layer run a pass without holding the `Agent` that owns the seam, avoiding a reference cycle. `compact()`/`fold_boundary()` signatures are unchanged, so C12's registry and the native runtime were untouched.
- A behavior change the lane chose deliberately and flagged: a seam-layer error now closes the turn rather than returning through a bare `?` and leaving `turn/start` unclosed — which the old auto-compaction failure path did.
- Not migrated, correctly: `context_estimate()` at `TurnFinished` is a settlement-time UI report, not an interception point; moving it would invent a seam the row does not name.

Mutation verification — nine tried, and the two that survived exposed blind tests:

- A synthetic layer that **changes nothing** cannot detect a short-circuit that dispatches the stale request. The rebuild mutation was caught only by the pre-existing auto-compaction test; the lane's own seam test was blind until its layer marked the request it claimed to have replaced.
- `cancel.child_token()` auto-propagates the turn token, so an explicit `operation.cancel()` only matters for the **independent caller** token — removing it survived until a test parked a layer and released it through the caller token specifically.
- A third finding worth keeping: a test that waits unbounded on a `Notify` does not *fail* under a mutation, it **hangs** — which reads as infrastructure trouble rather than a caught defect. Bound every wait a mutation is expected to break.

Gate: `cargo fmt -p heycode-agent -- --check`, `cargo clippy -p heycode-agent --all-targets -- -D warnings` — clean. `cargo test -p heycode-agent --no-fail-fast` — **105 passed** (21 unit, 84 integration). `cargo test -p heycode-cli --test main composition` — 29 passed, proving no service was added and the exact live-inventory audit is undisturbed.

Deliberately unbuilt: no `Abandon` verdict on the request seam (pre-step already stops turns; no consumer), and `MAX_PROVIDER_NATIVE_CONTINUATIONS_PER_TURN` stays hardcoded rather than moving onto pre-step — that is `A09`'s row.

## 2026-08-29 — TEL01: session-local usage/cost/latency projection

Tracker: `TEL01` complete. Row verbatim: "Session-local usage/cost/latency projection … `/usage` works with telemetry disabled". Rollup: **124 complete · 6 active · 162 not started · 0 blocked**. Buckets: 68 · 4 · 85 · 5.

Outcome:

- `project_usage(events)` derives per-turn token usage, route and timing from the durable log alone. It observes no live request, keeps no counter and needs no service — which is precisely what makes the acceptance ("works with telemetry disabled") hold rather than being asserted. A test projects the same slice twice and requires identical output.
- The projection is **neutral by dependency**: `heycode-session` sits below `heycode-llm`, so it reports tokens and instants and prices nothing. A Consumer joins it to `ModelPricing`/`RequestCost` from P12. This is the same boundary the request projection keeps by emitting neutral `WireMessage`s, and it is what lets each turn be priced by whichever model actually served it.
- A session spans routes, so `routes()` returns every distinct provider/model in first-seen order and no consumer can assume one price covered the session. Within a turn the **last** request header wins, because that is what the turn actually used after any re-route.
- Attribution is by turn index rather than adjacency — a log's turn events are not guaranteed contiguous — and a test interleaves two turns to prove it.
- Timing is derived defensively: `duration_ms` uses `checked_sub`, so a clock that went backwards yields no duration rather than a wrapped one; an unsettled turn (process died mid-turn) is `TurnOutcome::Open` with no duration while its reported work still counts.

The C11/P12 honesty rules carry over and are mutation-verified:

- A step that reported no usage increments `unreported_steps` rather than adding zero; a turn where nothing reported stays `None`. Two deliberate mutations — dropping the unreported counter, and defaulting an absent usage to zero — turn exactly those tests red.
- A session total is therefore a **lower bound** unless `is_complete()`. "Nothing to report" is complete; "reported nothing" is not.

Gate: `cargo fmt -p heycode-session -- --check`, `cargo clippy -p heycode-session --all-targets -- -D warnings` — clean. `cargo test -p heycode-session --no-fail-fast` — **95 passed** (31 unit, 64 integration).

Prior full workspace gate this session: **1179 passed / 0 failed**, fmt and clippy clean.

## K09 — plugin activation transaction and health result

Row: `| [x] | K09 | P2 | Add plugin activation transaction and health result | K03,K04 | Partial contributions never publish |`

Each plugin now activates inside a `Context` transaction. `begin_activation`
captures a private `ContextFingerprint`; a failure at any stage (`Admission`,
`Declaration`, `Apply`, `Commit`, `Rollback`) removes the keys that plugin's
`provide` inserted, truncates the inventory to its pre-activation length,
disposes only that plugin's effects LIFO — then **re-captures and compares**.
Residue becomes `CoreError::BrokenActivation { plugin, residue, cause }`, so a
rollback that did not work fails loud instead of being trusted. Ordinary
failures return the original error unchanged, keeping GOTCHAS #28 intact and
every existing workspace error assertion passing.

"Nothing visible" is proven over all six dimensions a plugin can change —
services map, exact inventory rows, recorded `AppliedPlugin`s, descriptors and
scopes, pending-effect count, total listener count — by a test that provides a
service, declares a static row, contributes a dynamic row, registers two effects
and a listener, then fails, and asserts the whole fingerprint is unchanged. An
external Consumer separately observes the shared inventory as it was.

`compose`/`compose_scoped` keep their signatures as thin wrappers over
`compose_activation`/`compose_scoped_activation`, so no other crate changed.

Thirteen mutations were run and every one turned a named test red. One found a
real hole of the opposite kind: rollback truncated `ctx.plugins`/descriptors,
and deleting those lines broke nothing because nothing can fail after
`record_plugin` commits. The dead code was removed rather than left as unpinned
decoration; the fingerprint still watches that dimension.

**The A02 join.** The transaction covers what the `Context` owns, so a plugin
mutating an earlier plugin's published state is outside it — and A02 made that
reachable by making seams the plugin extension point. `Waterfall::push_effect`
was added: a contributed layer is removed by the plugin's own effect on rollback
or shutdown, with a **weak** handle so unwinding after the chain is dropped is a
no-op. `push_shared` is now documented as chain-lifetime only. Three mutations
(disposer never registered, retain-everything, over-remove) each turn the new
test red; a second test covers the dropped-chain path. This mirrors the
`on`/`on_effect` line the `EventBus` already drew.

**Composition root.** A successful report says only "every plugin activated",
which `ctx.plugins()` already says — its information is in the failure. So
`compose_world` now composes through `compose_scoped_activation` and, on error,
adds one line naming the failing plugin, its stage, and how many later plugins
never ran. `describe_activation_failure` is a pure function so it is testable
without a broken production world; three mutations (blame the first row, count
every row, describe a non-failure) each turn the right test red.

Gates: `cargo fmt --all --check` clean · `cargo clippy -p heycode-core -p heycode-cli
--all-targets -- -D warnings` clean · `heycode-core` 30 lib + 20 it, 0 failed
(was 25 + 13) · full workspace suite green.

Known limits, recorded rather than closed: `ActivationReport` is not
`Serialize`, which `doctor --json` would need; and no generic sweep can detect a
layer leaked through `push_shared`, because a chain lives behind a type-erased
service — the discipline in AGENTS §3 is what covers it.

## K10 — plugin reload generation model

Row: `| [x] | K10 | P2 | Add plugin reload generation model | K09 | Successful reload swaps once; failed reload keeps last good |`

`crates/heycode-core/src/generation.rs`: `Generation`, `GenerationRegistry`,
`ReloadOutcome::{Swapped, Kept}`, `ReloadRejected`.

Both acceptance clauses come from one ordering decision. `reload` composes the
candidate through K09's activation transaction **before** taking any lock and
before retiring anything, so a candidate that fails costs the live world nothing
— not even a moment of unavailability. Only a world that activated cleanly is
allowed to replace the live one, under a single write lock, and the generation
advances by exactly one.

Generations count worlds that ran, not attempts: three failed reloads leave the
registry on generation 1, so the number always describes what is actually
serving requests.

Readers take `Arc<Context>`, so a swap never pulls a world out from under work
in flight. The retired world is parked and shut down only when `Arc::get_mut`
succeeds — that success is the proof no one is reading. Deferred disposal is not
skipped disposal: `pending_disposal()` and `sweep_retired()` make a world held
by a reader that never let go visible instead of leaked.

Six mutations, each turning the right named test red: increment on failure;
swap the generation but keep serving the old context; increment by two; drop the
retired handle instead of parking it; drop it without `shutdown`; retire before
composing. A seventh — dispose while readers remain — **could not be written**:
it needs `&mut` from a shared `Arc`, which needs `unsafe`, which the workspace
forbids. That error is unrepresentable rather than untested.

Gates: `cargo fmt --all --check` clean · `cargo clippy -p heycode-core
--all-targets -- -D warnings` clean · `heycode-core` 36 lib + 20 it, 0 failed.

Known limit, recorded rather than closed: the model has no production consumer.
`heycode-cli` composes once and never reloads, and no tracker row depends on K10.
It is a correct, proven model waiting for a `/reload` surface to use it.

## Q08 — live test artifact schema and secret redaction

Row: `| [x] | Q08 | P1 | Live test artifact schema and secret redaction | Q02,S15 | Captures metadata/outcome, never credentials/content unless opted |`

New crate `heycode-live-artifact`: `LiveArtifact`, `ArtifactRecorder`, `RouteId`,
`LiveOutcome`, `FailureClass`, `SkipReason`, `ContentPolicy`, `ContentCapture`,
`ScreenedText`, `ArtifactFault`, `ARTIFACT_SCHEMA_VERSION`.

The acceptance names two things, and the design keeps them independent.
**Content** is withheld by default and released only by an explicit
`ContentPolicy::RecordOptedIn`. **Credentials** are refused always — including
from an opted-in run, because asking for response bodies is not asking to
publish an API key. One flag covering both is the failure this row exists to
prevent.

`ScreenedText` has no constructor but the screen, so "an artifact cannot carry a
credential" is a property of the type. `FailureClass` and `SkipReason` are
closed sets for S15's reason: a provider error message routinely echoes the
request with headers, so the class is recorded and the message stays in the
runner's logs. `RouteId` carries provider and model but no endpoint URL — a URL
is the one metadata field that routinely holds a token in its query string, and
withholding it costs nothing. A refused body yields no artifact at all rather
than one with a field quietly dropped.

The credential screen is **shared** with S15, not reimplemented:
`heycode-settings::screen_text_for_credentials` was added and exported for this.
One list of what a credential looks like; a second copy that drifts is how a
leak ships.

Ten mutations, each red on a named test: opt-in skips the screen; withholding
screens what it drops; withholding records anyway; notes bypass the screen; the
default policy records; a future schema version is accepted; an unmeasured
latency becomes zero; route validation ignores control characters; the screen
matches only whole-string credentials (the embedded case is the realistic one);
the multi-line PEM check is dropped.

Gates: `cargo fmt --all --check` clean · `cargo clippy -p heycode-live-artifact -p
heycode-settings --all-targets -- -D warnings` clean · `heycode-live-artifact` 15
passed · `heycode-settings` 29 passed.

Unblocks Q09 and QLIVE01. Like K09's report and K10's registry, it has no
producer yet — infrastructure lands before its consumers in this dependency
order, and that is recorded rather than hidden.

## Gate repair — host binary scanning, not a regression

Eight tests (`heycode-runtime-claude` ×7, `heycode-cli` acp ×1) began failing with
`Unavailable`/`Elapsed` after passing an hour earlier. Cause: executing a newly
written binary costs 11-23 seconds on this machine; re-executing the same file
costs 5ms. A one-line `/bin/sh` script measured 0.00s user, 0% CPU, 23s wall.
Both suites write a fresh executable — a fixture script, and the freshly built
`heycode` binary — and then time an interaction against 5s and 10s budgets.

My changes were ruled out first: `heycode-runtime-claude` does not depend on
`heycode-settings`, and the `heycode-core` delta was one unreferenced new module.

Fixed in the tests, not the product: each suite now executes its program once,
untimed, before anything measures it. `heycode-runtime-claude` 12/12 and the acp
tests 2/2 pass.

**Left open deliberately:** `VERSION_TIMEOUT` is 5s in production, so a user who
just installed or updated `claude` on a scanning host will be told the runtime
is unavailable when it is fine — and a timeout is classified `Unavailable` when
it is honestly `Unknown`. Recorded in GOTCHAS #155 for an owner decision, since
widening the budget also delays detecting a genuinely hung binary.

## MCP04 — OAuth PKCE and credential records

Row: `| [x] | MCP04 | P1 | OAuth PKCE and credential records | MCP03,S08 | Auth/refresh/logout/reauth state pass |`

`crates/heycode-mcp/src/oauth.rs`: `CodeChallengeMethod`, `PendingAuthorization`,
`TokenSet`, `TokenExpiry`, `Freshness`, `McpAuthState`, `McpOAuthClient`,
`OAuthRecords`, `TokenExchange`, `OAuthFault`, `MCP_OAUTH_CREDENTIAL_KIND`.

**PKCE.** `CodeChallengeMethod` has exactly one variant, so there is no path to
`plain` to negotiate down to. A server offering only `plain` is refused, and so
is one that advertises nothing — unknown is not supported. The verifier draws
366 bits from the platform CSPRNG (three v4 UUIDs at 122 random bits each),
past RFC 7636's 256-bit recommendation, and the challenge is asserted to be
S256 of it rather than assumed.

**The four states.** Authorize, refresh, logout, reauthorize each have a test.
`complete` takes the pending authorization **by value**, so a verifier cannot be
replayed against a second code. A refresh that omits a new refresh token keeps
the old one (RFC 6749 §6); dropping it would silently downgrade the session to
one-shot. Logout is total and idempotent, and a refresh afterwards fails rather
than resurrecting the session. A callback's `state` is compared before its code
is read, proven by a callback wrong on both counts reporting `StateMismatch`.

**Credential records.** One record per server, not one per token: the stated
expiry travels with the tokens it describes, and logout is a single delete.
Delete-one is provably total; delete-three-and-hope-none-is-orphaned is not, and
a forgotten refresh token after logout is a live credential the user believes
they revoked. A corrupt record reads as absent so it sends the user through a
fresh authorization instead of wedging the server.

**Token exchange.** A single auditable function puts the verifier and refresh
token on the wire, form-encoded outside the unreserved set. A non-2xx answer is
`Denied` carrying no server text — a token-endpoint error body is
attacker-influenced and echoes the request — while a transport failure is
`Unreachable`, because "the network broke" and "the server refused you" lead to
different next steps.

23 mutations, every one red on a named test: 12 on the state machine (accept
`plain`; accept an empty advertisement; skip the state check; read the code
first; send the verifier as the challenge; no-op logout; drop the surviving
refresh token; refresh while unauthenticated; absent `expires_in` means expired;
one fixed verifier; Debug prints the verifier; `refreshable` always true), 5 on
records (refresh token not persisted; unknown expiry stored as zero; empty
access token loads; no-op clear; missing stored expiry becomes the epoch), and 6
on the exchange (verifier omitted; challenge sent instead; no percent-encoding;
non-2xx accepted; transport failure reported as denial; wrong grant type).

Gates: `cargo fmt --all --check` clean · `cargo clippy -p heycode-mcp --all-targets
-- -D warnings` clean · `heycode-mcp` 8 lib + 88 it, 0 failed (was 8 + 60).

Unblocks MCP05 and MCP15. Out of scope by row boundary and left to those rows:
authorization-server discovery and dynamic client registration (MCP05), and the
local callback listener plus browser launch (MCP10).

## MCP10 — CLI/TUI management and diagnostics

Row: `| [x] | MCP10 | P1 | CLI/TUI management and diagnostics | MCP01,U03 | add/list/auth/test/edit/enable/remove parity |`

(Dependency corrected from `MCP01,U12` — see the tracker cycle note and
GOTCHAS #157.)

`crates/heycode-mcp/src/management.rs`: `McpOperation`, `McpManagement`,
`StoredServer`, `McpDefinitionStore`, `McpHealth`, `McpProbe`, `McpStatusRow`,
`McpManagementError`, `SettingsBackedStore`, `settings_namespace`,
`settings_definition`, `mcp_management_plugin`.
`crates/heycode-cli/src/mcp_cli.rs`: `McpCommand`, `parse`, `run`, `usage`,
`compose_management_world`, plus the `heycode mcp` mode in `main.rs`.

**Parity is structural.** `McpOperation` is a closed enum — deliberately not
`#[non_exhaustive]` — that both surfaces match exhaustively, over a single
`McpManagement` implementing each operation once. An eighth operation stops the
build in every surface that has not handled it. `McpHealth` keeps
`#[non_exhaustive]` with a default arm falling to unknown: the two enums want
opposite defaults because an unhandled operation must break the build while an
unrecognized health state must degrade safely.

**Persistence** is the `mcp-servers` settings namespace, registered by its own
plugin so the base `mcp-registry` grows no settings dependency. The
administrator lock is enforced at the store: a managed-layer definition arrives
marked, refuses every mutating operation, and is never copied into the user
layer on persist — the escape route that would otherwise exist.

**`heycode mcp` composes two plugins, not the world.** The first wiring composed
everything and `heycode mcp list` failed with "no API key for `deepseek`". Verified
end to end with no credential configured: add, list across separate processes,
enable `--off`, edit, duplicate refusal, unknown-server refusal, remove, and
`settings.toml` written. Exit status is 1 on failure, 0 on success.

That failure also validated K09 in production — the error named the failing
plugin, its stage, and the 19 plugins that never ran.

12 mutations, each red on a named test: add overwrites a duplicate; edit resets
`enabled`; remove of an unknown succeeds; a managed definition accepts mutation;
health defaults to reachable; **persist copies managed definitions into the user
layer**; the managed layer loads without its lock; layer precedence reversed; an
absent `enabled` defaults to disabled; the CLI accepts both `--command` and
`--url`; `add` dispatches to `edit`; `enable` ignores `--off`.

A test premise was wrong in the safe direction: I expected a malformed settings
row to be skipped, and the schema rejects the whole section naming the offender.
That is the better behaviour, so the test now pins it, and the store's skip
branch is pinned separately by a case the schema allows but the model refuses.

Gates: `cargo fmt -p heycode-mcp -p heycode-cli -- --check` clean · `cargo clippy -p
heycode-mcp -p heycode-cli --all-targets -- -D warnings` clean · `heycode-mcp` 8 lib +
109 it · `heycode-cli` 13 lib + 62 it, 0 failed.

Unblocks U12 (the MCP panel), which now calls the same `McpManagement`.

Noted, not yet a defect: `mcp-management` was added to the **default** plugin
set, so a user with an explicit complete `[profile] plugins` list will not have
the `mcp-servers` namespace in their main world. `heycode mcp` is unaffected — it
composes its own two-plugin world — but U12's panel will read the namespace from
the main world, so U12 should carry the schema migration that inserts
`mcp-management` into complete profiles. No migration was added here because
nothing in the main world reads the namespace yet, and a migration with no
consumer is a change users pay for and nobody uses.

**Correction (2026-08-31).** That last clause is no longer true. `McpPlugin::apply`
in product mode now adopts the session's configured `[mcp.servers]` rows into
`McpManagement` and merges `McpManagement::connectable()` into the connection
set, so the main world does read the namespace: a server added by `heycode mcp add`
takes effect in the next session and configuration wins a name collision. The
migration question the paragraph above deferred is therefore live again for
users holding an explicit complete `[profile] plugins` list. What remains
one-directional is the standalone CLI: `compose_management_world` never loads
`Config`, so `heycode mcp list/remove/enable` still sees only added servers while
the in-session `/mcp` panel sees both (GOTCHAS #158).

## POA01, PAWS01, PGCP01, PMM01, PZA01 — five delegated provider/authorization lanes

Rows:
- `| [x] | POA01 | P1 | OpenAI API auth/catalog plugin | P03,CAT02,S09 | Models and account-capable features refresh |`
- `| [x] | PAWS01 | P1 | AWS auth/profile/region provider | S08 | API key and SDK chain status validated without secrets |`
- `| [x] | PGCP01 | P1 | ADC/project/location auth profile | S08 | Account/project/location health checked |`
- `| [x] | PMM01 | P1 | MiniMax provider profiles and distinct credential kinds | P04,P05,S04 | PAYG and Token Plan cannot be confused |`
- `| [x] | PZA01 | P1 | General and Coding Plan provider profiles | P04,S04 | Endpoints/credentials are distinct and visible |`

Five new crates: `heycode-provider-openai`, `heycode-authorization-aws`,
`heycode-authorization-gcp`, `heycode-provider-minimax`, `heycode-provider-zai`.
207 tests across them.

**None of these lanes delivered a report.** All five went idle after finishing
and did not respond to two rounds of messages, so every claim below was verified
by me directly rather than accepted on trust.

Gates re-run independently for each crate: `cargo fmt -p <crate> -- --check`
clean, `cargo clippy -p <crate> --all-targets -- -D warnings` zero errors,
`cargo test -p <crate> --no-fail-fast` — 19 / 49 / 76 / 37 / 26 passing, 0
failed.

Acceptance verified against the tests that pin it, not against the titles:
- **PAWS01** resolves each documented credential-chain shape to its own source,
  refuses half a static key pair rather than reading it as an empty
  environment, treats unreadable shared config as blocking discovery rather
  than absence, and — the acceptance's "without secrets" — has two tests
  asserting that a report over secret-bearing environment variables and files
  publishes none of their values.
- **PGCP01** gives the project three distinct states: confirmed by the host,
  *unset*, and *undetermined* — with an explicit test that an undetermined
  credential document leaves it undetermined and **not** unset. Location
  precedence is ordered across every documented source, and the probe reads
  only three non-secret paths and never a token.
- **PMM01** makes the two products mutually exclusive by admission: the Token
  Plan refuses a secret without the documented prefix, and pay-as-you-go refuses
  one carrying it. Debug names the product and redacts the secret; admission
  failures never repeat the rejected value.
- **PZA01** proves no two documented base URLs are shared between the plans, a
  plan credential refuses the other plan's default reference, and every
  documented base URL is an absolute credential-free HTTP URL.
- **POA01** separates model entitlement from key acceptance, publishes entitled
  models for an account lacking the provider default, refuses a whole generation
  on a malformed row, and never lets an unsafe configured model id reach a URL.

Factual claims checked against live documentation rather than accepted:
- Every cited documentation URL sampled returns 200 — the citations are real
  pages, not fabricated.
- Z.ai's own `devpack/quick-start` names exactly the base URLs the crate
  encodes (`api/v1`, `api/coding/paas/v4`, `api/anthropic`).
- MiniMax's `token-plan/claude-code` names both hosts the crate handles — the
  global `api.minimax.io` and mainland `api.minimaxi.com`.
- The `sk-cp` Token Plan prefix is confirmed on the two pages the crate cites
  for it (`token-plan/openclaw`, `token-plan/other-tools`). My first check
  fetched three other pages and found nothing; the lane's citations were the
  precise ones.
- No provider crate hardcodes model ids — catalogs supply them, which is the
  right call and removes the largest fabrication surface.
- PMM01 deliberately publishes **no** prefix for the pay-as-you-go key, on the
  grounds that MiniMax documents none and "an unverified prefix would reject
  valid keys". That is the discipline these lanes were briefed on, applied
  without being asked.

Process note: a shared scratchpad path (`<scratchpad>/pristine/`) let two lanes
overwrite each other's mutation-testing snapshot, and one lane's crate source
was replaced with the other's. It reported the collision clearly, rebuilt from
documentation rather than memory, and its final crate is its own. Lane
snapshots are now required to be lane-unique — GOTCHAS #159.

## PL06 — CLI/TUI plugin lifecycle

Row: `| [x] | PL06 | P1 | CLI/TUI plugin lifecycle | PL02,U03 | install/enable/disable/update/rollback/remove work |`

(Dependency corrected from `PL02,U13` — see GOTCHAS #157.)

`crates/heycode-extensions/src/lifecycle.rs`: `PluginOperation`, `PluginLifecycle`,
`PluginState`, `PluginStateStore`, `InstalledVersions`, `LifecycleError`,
`SettingsBackedStateStore`, `settings_namespace`, `settings_definition`,
`plugin_lifecycle_plugin`. `crates/heycode-cli/src/plugin_cli.rs`: `PluginCommand`,
`parse`, `run`, `usage`, `compose_lifecycle_world`, plus the `heycode plugin` mode.

**PL02 stores versions; it has no opinion about which one is live.** That
opinion is this module's, and keeping them separate is what makes rollback cheap
— the cache already retains prior versions, so rolling back re-points a
reference rather than re-downloading anything.

Same parity mechanism as MCP10: a closed `PluginOperation` both surfaces match
exhaustively. `list` is deliberately **outside** it — a query is not a lifecycle
transition, and putting it in the set would force every surface to treat a read
as a state change.

Design decisions worth naming:
- `previous` holds exactly one step. A rollback is an escape from a bad update,
  not a version-control system, and offering arbitrary history would promise
  more than the cache's retention policy can keep.
- Rollback **swaps**, so a rollback can itself be rolled back — an operator who
  rolls back by mistake is one command from undoing it.
- A remembered version the cache has since pruned fails loudly rather than
  leaving a reference to something that cannot activate. Retention is the
  cache's policy, not this layer's assumption.
- An update to an uncached version is checked before anything mutates, so a
  failed transition leaves both the active version and the rollback target
  intact — K10's rule applied to packages.
- `install` refuses an already-installed plugin: conflating install with update
  would let an install silently discard the rollback target.

This `enabled` flag is **not** K05's `PluginDirective`, which selects composed
plugins by factory id inside a profile. This one records installed marketplace
packages by two-segment id. PL03 is the bridge that reads it to decide which
installed package contributes to a real composition.

8 mutations, each red on a named test: install overwrites an existing plugin;
install records an uncached version; update forgets the version it left; update
accepts an uncached target; update to the active version is accepted; rollback
does not swap; rollback ignores a pruned previous version; remove of an unknown
plugin succeeds silently.

`heycode plugin` composes two plugins (settings-file, plugin-lifecycle) rather than
the product world, verified end to end with no credential configured: usage,
`list` on an empty world, refusal of an uncached install, malformed id and
version refused at the boundary, unknown operation listing the alternatives.
Exit status 1 on failure, 0 on success.

Unlike `mcp-management`, `plugin-lifecycle` was **not** added to the default
plugin set. The asymmetry is deliberate: U12 is being built now and will read the
`mcp-servers` namespace from the main world, whereas nothing reads `plugins`
until PL03. A plugin activated at every startup for no consumer is a cost users
pay for nothing.

Gates: `cargo fmt -p heycode-extensions -p heycode-cli -- --check` clean · `cargo
clippy -p heycode-extensions --all-targets -- -D warnings` clean · `heycode-extensions`
5 lib + 48 it · `heycode-cli` 13 lib + 69 it, 0 failed.

## S14 — settings UI contribution registry

Row: `| [x] | S14 | P2 | Settings UI contribution registry | S01,U03 | Provider/plugin settings render from schemas and custom panels |`

`crates/heycode-ui/src/settings_ui.rs`: `SettingsField`, `FieldOrigin`,
`UnrenderableReason`, `SettingsForm`, `SettingsSurface`, `SettingsUiRegistry`,
`derive_form`.

Both halves of the acceptance: a plugin gets a settings surface for free by
declaring a schema, and can opt out into its own panel without the host learning
anything about that plugin. Both are UI-neutral — this crate decides *what* to
show, `heycode-tui` decides how.

**A secret has nowhere to be.** `SettingsField::Secret` carries `configured:
bool` and no value field, so a secret cannot reach a renderer, a log or a
snapshot by accident, and no later edit to a render path can reintroduce the
bug. Secrecy is decided by two screens in order of authority: a wire-exposed
namespace's `redacted_paths` (S15 already screened it, and it catches a field
declared secret under an innocuous name), then the shared
`names_credential_material` recognizer — the same single list Q08 uses.

An unrenderable construct becomes a visible row rather than being dropped, a
managed value renders read-only rather than editable-then-rejected, and a choice
holding a value outside its options reports nothing selected rather than
blessing an invalid value.

9 mutations. Seven died immediately; **two survived and both were defects in my
own tests** — see GOTCHAS #160. One was a redundant managed-layer check that no
test could distinguish from `managed_locks`; the redundancy was removed rather
than papered over. The other was a precedence test whose layers never
overlapped, so reversing the order changed nothing; it now puts one path in all
three layers, one in two, one in one, and one in none.

Two accessors were added to `heycode-settings` while exploring and then **removed**
when the public `wire_projection()`/`managed_locks()` turned out to be
sufficient. Unused public API is the K09 lesson.

Gates: `cargo fmt -p heycode-ui -p heycode-settings -- --check` clean · `cargo clippy
-p heycode-ui -p heycode-settings --all-targets -- -D warnings` clean · `heycode-ui` 2 lib
+ 11 it · `heycode-settings` 29, 0 failed.

## TEL02, U12 — two delegated lanes, verified

Rows:
- `| [x] | TEL02 | P2 | Telemetry service and local-off provider | K01 | No outbound telemetry by default |`
- `| [x] | U12 | P1 | MCP panel | U03,MCP10 | Status/auth/tools/resources/prompts/actions work |`

Neither lane sent a report; both were verified directly. Gates re-run
independently: fmt clean, clippy clean under `-D warnings`, `heycode-telemetry` 34
tests and `heycode-tui` 78 tests passing.

**TEL02 proved its guarantee structurally rather than by configuration**, which
is what the row deserved. `tests/no_egress_by_construction.rs` audits the crate
itself: that its manifest names nothing that can reach off-process, that no
source file does either, that it inherits the workspace lints forbidding
`unsafe`, and that the public surface offers no constructor supplying its own
exporter. "No outbound telemetry by default" is therefore true because the
default provider has no way to send anything.

Scope stated precisely: the guarantee is that no code *in this crate* can reach
off-process, because it names only `heycode-core`, `heycode-settings`, `serde` and
`serde_json`. `tokio` does appear transitively via `heycode-core`, but telemetry
cannot call it without naming it. The lane also dropped its dev-dependencies
with a reasoned comment — an unused dependency in a crate whose guarantee is
"it can only name four things" is not free.

Its tests catch things I would not have specified: a label cannot be
deserialized past its constructor (validated newtypes routinely leak validation
through `Deserialize`), a label rejects an embedded bearer-token shape, a second
provider of the key fails composition instead of layering, a duration saturates
rather than wrapping, and a fault never carries the offending text.

**U12 drives `McpManagement` rather than duplicating it**, matching exhaustively
on the closed `McpOperation` so parity with the CLI is structural. Its tests pin
the two properties I was most concerned about — a managed server shows its lock
and is never offered a mutating action, and an unrecognized health state reads as
unknown and never as reachable — plus one it found itself: an HTTP target is
rendered without its query or userinfo, because a URL is where a credential
hides in plain sight.

Honest about what exists: **tools are live; resources and prompts report
themselves unsupported rather than empty**, with the module naming MCP08 and
MCP09 as the rows that will flip them. An empty prompts list would have implied
the server has no prompts, which is a lie. Status, auth, tools and actions are
live; the row's own dependencies (U03, MCP10) are met, so it is complete
relative to what the build implements.

## PLM01 — LM Studio endpoint/auth/health plugin

Row: `| [x] | PLM01 | P1 | LM Studio endpoint/auth/health plugin | P03,P04,P05 | Server/version and compatible protocols detected |`

New crate `heycode-provider-lmstudio`, 42 tests, 23/23 mutations killed. Verified
directly: gates re-run, and the lane's own report is unusually strong evidence.

The design turns on two facts the lane established rather than assumed: LM
Studio publishes **no** version, health or status endpoint (an open, unanswered
issue), and its server answers `200 OK` on paths it does not route. So a status
code is only a gate and **the documented body shape is the evidence**. Health is
four outcomes — `NotRunning` (an ordinary local state, returned as a verdict and
never an error), `Indeterminate`, `RunningUnrecognized`, `Running` — and a
version is never claimed exactly: `LmStudioVersion` is `Unknown | AtLeast`, with
`AtLeast("0.4.0")` the only citable bound.

Two rules I want recorded because they are the row read correctly: the crate
**never emits `Unsupported` at all**, since a probe can prove a surface present
but never absent; and a version lower bound is not a protocol observation, so
`OpenAiResponses` and `AnthropicMessages` stay `Unknown` (both are POST-only and
probing one could load a model). A mutation promoting an unobserved protocol
from a version bound turns two named tests red.

The lane also reported that **its training data was wrong on the load-bearing
fact** — the native REST API is `/api/v1/*` since 0.4.0, not `/api/v0/*`. Had it
not checked, the version bound and the primary probe would both have been wrong.
That is the entire reason the briefs demand primary sources.

## Composition wiring for five delegated crates

Five lanes correctly declined to touch the composition root and told me exactly
what to wire. Now done in `heycode-cli`:

- `provider-openai` + `catalog-openai` (POA01), with the same
  `cfg.llm.provider == "openai"` reference override the Anthropic plugin uses.
- `authorization-aws` (PAWS01) — contributes flow `aws-bedrock-api-key`,
  provides `aws-auth`.
- `authorization-gcp` (PGCP01) — provides `gcp-auth`, and deliberately
  contributes **no** authorization flow: ADC has no secret to persist, and
  minting one would be fiction.
- `provider-lmstudio` (PLM01) — provides `lmstudio`.
- `telemetry-local-off` (TEL02) — provides `telemetry`.

`BUILTIN_SERVICE_KEYS` gained the four new keys in composition order, and the
exact-inventory audits were updated for the plugin list, the authorization-flow
list, the service-key map and the descriptor families. All 29 composition audits
pass; `heycode doctor --composition` runs the enlarged world; `heycode-cli` is 13 lib +
69 it, 0 failed.

## Q08 correction — the read path had a hole

The TEL02 lane, reviewing Q08 as a reference implementation, noticed
`ScreenedText` was `#[serde(transparent)]`. It was right: `LiveArtifact::from_json`
accepted a hand-authored artifact carrying `sk-proj-…` straight past the screen.
My Q08 entry claimed the type made that impossible; the claim held on the write
path only, and all ten of my mutations had been write-path mutations.

Fixed with `#[serde(into = "String", try_from = "String")]`, pinned by
`a_hand_authored_artifact_cannot_smuggle_a_credential_past_the_screen`, and
mutation-verified: making `try_from` skip the screen turns exactly that test red.
16/16 green. Recorded as GOTCHAS #161, including the general obligation to audit
every validated newtype in the workspace that derives `Deserialize`.

## O08 — hook service and command protocol

Row: `| [x] | O08 | P2 | Hook service and command protocol | K01,E03 | Pre/post lifecycle, timeout, trust and disposal work |`

New crate `heycode-hooks`: `HookService`, `Hook`, `HookPhase`, `HookEvent`,
`HookOutcome`, `HookFault`, `SERVICE_HOOKS`, `HOOK_TIMEOUT`, `hooks_plugin`.
14 tests, 11 mutations, all killed.

Three of the four things the row names exist because a hook is arbitrary
third-party code running at a moment heycode chose:

- **Lifecycle.** A `Pre` hook exiting non-zero refuses the operation; the
  identical exit from a `Post` hook cannot, because the operation already
  happened. The phase decides, not the exit code, and `can_refuse()` puts it in
  the type.
- **Timeout.** `HOOK_TIMEOUT` is fixed and deliberately not per-hook
  configurable — a hook author cannot extend their own leash. Classification is
  by the typed `ProcessExit`, never by parsing an error string.
- **Trust.** A project-scoped hook stays inert unless the workspace is
  affirmatively `Trusted`; `Unknown` is not trusted, which is K12's rule stated
  positively. A user's own hook is ungated, because the gate is about code that
  arrived with the checkout.
- **Disposal.** Registration is effect-only. A hook that outlives its owner is
  code running for a plugin that is gone, so there is no `register_shared`
  equivalent to misuse.

A broken hook is not an outage: a fault still proceeds and does not stop later
hooks, while a refusal does stop them — a refused operation must not keep
running the hooks meant to observe it.

Two of my own tests were decoration and mutation found both; see GOTCHAS #164.
The cancellation test asserted the outcome but not that nothing launched, so
removing the pre-launch check survived — it now counts launches through an
injected backend, with a companion test proving the counter counts. The timeout
test *hung* under its mutation rather than failing, and now bounds its own wait.

Wired into the default world as `hooks`, ordered after `shell-local` which it
injects, with `SERVICE_HOOKS` in `BUILTIN_SERVICE_KEYS` and all four audits
updated. The trust decision is resolved at composition and defaults to
`Unknown`. No profile migration: nothing injects `hooks`, so existing exact
profiles still compose.

Gates: fmt clean · clippy clean under `-D warnings` · `heycode-hooks` 14 passed ·
`heycode-cli` 13 lib + 69 it, 0 failed.

## Tracker correction — PL03 was understated, not cyclic

PL03 (`Declarative skills/commands/agents/hooks/themes/providers`) listed `PL01`
alone, but **no hook registry and no theme registry existed**: O08 builds the
first and U20 owns the second. "Each contribution activates/disposes in real
composition" was unsatisfiable for two of six kinds while the tracker showed the
row as ready. Corrected to `PL01,O08,U20`.

This is the mirror image of the four cycles: a cycle makes a reachable row look
blocked; an understated dependency makes a blocked row look ready. Adding an
edge makes a row *less* reachable, so the bar is higher than for removing a
cycle — the justification here is two registries verified absent by grep, not a
judgement call. O08 is now complete, so PL03 needs only U20.

## Nine delegated rows, verified and marked

`| [x] | P06 | P1 | Gemini GenerateContent protocol adapter | P02 | Parts, function calls, thought signatures and usage normalized |`
`| [x] | P07 | P1 | Bedrock Converse protocol adapter | P02 | AWS event stream, tools and usage normalized |`
`| [x] | MCP08 | P2 | Resources and subscriptions | MCP03 | List/read/change and UI inspect work |`
`| [x] | MCP09 | P2 | Prompts and server instructions | MCP03 | Prompt args and instructions available to Consumers |`
`| [x] | PAWS03 | P1 | Runtime ListFoundationModels discovery | CAT02,PAWS01 | lifecycle/modalities/stream/inference metadata retained |`
`| [x] | PGCP02 | P1 | Gemini catalog/profile | CAT02,PGCP01 | Accessible model capabilities normalized |`
`| [x] | PZA02 | P1 | Live/maintained GLM catalog | CAT02,PZA01 | Limits/capabilities/retirement normalize |`
`| [x] | PMM02 | P1 | OpenAI/Anthropic model discovery | CAT02,PMM01 | Both list endpoints normalize current models |`
`| [x] | U13 | P1 | Plugin panel | U03,PL06 | Provenance/permissions/enable/update state work |`
`| [x] | PLM02 | P1 | Native model list and capability mapping | CAT02,PLM01 | loaded/downloaded/tool-trained state visible |`

Verified independently per crate — fmt, clippy `-D warnings`, tests re-run:
`heycode-llm` 335, `heycode-mcp` 219, `heycode-provider-aws` 62, `heycode-provider-google` 34,
`heycode-provider-zai` 42, `heycode-provider-minimax` 68, `heycode-provider-lmstudio` 72,
`heycode-tui` 98. Zero failures.

Findings from the lanes worth keeping:

- **P06** requires a thought signature only on the *first* parallel function
  call, which is the subtle half of the rule and is pinned by name.
- **PAWS03** kept output modalities, streaming support and inference types on a
  provider-owned row because `ModelDescriptor` has no field for three of the
  four kinds the row names — flattening them away would have satisfied the
  compiler and failed the row.
- **PGCP02** built no Vertex catalog and said why: Vertex's `PublisherModel`
  publishes no token limits, no capabilities and no supported-method list, and
  the shipped Google SDK reads none of them on that path. A generation with
  every capability Unknown answers nothing, and shipping it would look like
  coverage.
- **PZA02** probed for a Z.ai model-list endpoint, got a 401 that reads as
  "exists, needs auth", then ran the control: a nonsense path returns the
  identical 401. The auth gate fires before routing, so 401 proves nothing
  there. Without the control it would have shipped a parser for an endpoint it
  had no evidence exists.
- **PMM02** resolved an ambiguity I had recorded as unresolvable: the two list
  endpoints are not ambiguous, they have separate reference pages documenting
  *different* schemes. It scoped the method to `list_models_auth` so the
  resolution cannot leak into the inference path, which PMM03 still owns.
- **MCP08** found that `resources/subscribe` does not exist in the current
  `2026-07-28` revision — it was replaced by a `subscriptions/listen` stream. The
  subscription half of the row is legacy-era by necessity, recorded at the top of
  the module rather than discovered later.

## `UntrustedContentSource::Mcp`

Both MCP lanes refused to stamp server content `UNTRUSTED WEB CONTENT` and took
a worse API rather than a false provenance claim. Added `Mcp` with label
`MCP SERVER`, `mcp()`, and three tests. GOTCHAS #166.

## Composition wiring

`catalog-google`, `catalog-zai`, `catalog-zai-coding` and `catalog-lmstudio`
joined the default set, with `SERVICE_LM_STUDIO_MODELS` in
`BUILTIN_SERVICE_KEYS` and all four exact-inventory audits updated.
`heycode doctor --composition` reports **79 plugins ready**.

`catalog-bedrock` was deliberately **not** added: PAWS03 fails composition when
no AWS region is configured, by design and correctly, so it must be profile-gated
rather than universal. Recorded rather than worked around.

## Two decisions the lanes escalated

**PAWS02** could not be built where I assigned it — `heycode-provider-aws` already
imports `heycode-authorization-aws`, so a Mantle catalog in the authorization crate
is a dependency cycle, not a preference. The lane wrote no code and reported.
Moved to `heycode-provider-aws` under its own provider id `bedrock-mantle`: the two
endpoints have different protocol sets and different reachability (Mantle needs
no SigV4), and `CatalogRegistry` keys by provider id, so one id would force two
discoveries into one failure mode.

**POA02** needs a change in the shared `crates/heycode-llm/src/responses.rs`. The
lane stopped and argued it: a provider-owned wrapper could reject bad replays but
could not make replay *correct*, because the phase-dropping serialization lives
inside the shared function, and the row says "replays required items". Took
Option A; both `heycode-llm` lanes have landed, so the crate is clear.

## C13 — `/context` contributor projection

Row: `| [x] | C13 | P2 | \`/context\` contributor projection | C11 | Context budget explains each contributor |`

(Dependency corrected from `C11,U16` — see GOTCHAS #157.)

`crates/heycode-llm/src/context_projection.rs`: `ContextProjection`, `BudgetUse`,
`ContributorLine`. 10 tests, 6 mutations, all killed.

C11 measures an envelope; this explains one. The rule that shapes everything is
that an **uncounted contributor has no number**, so a budget containing one is a
lower bound and every quantity derived from it is a bound too:

- `BudgetUse::Measured` carries `used`/`remaining`; `BudgetUse::AtLeast` carries
  `used_at_least`/`remaining_at_most`. The names say which claim is being made,
  and there is no plain `percent()` accessor a caller could reach for without
  learning which it has.
- **No line reports a share while any contributor is uncounted** — including
  lines that were themselves counted exactly. A share of a lower bound
  overstates every counted contributor, because the denominator is too small.
- An unpublished context window yields `WindowUnknown`, not a zero window. No
  known headroom is not zero headroom.
- The one thing an incomplete measurement *can* prove is an overflow: uncounted
  contributors only add, so a lower bound past the window is a real overflow.
  `known_to_overflow()` returns true for that case and false for an incomplete
  measurement under the window, which proves nothing either way.
- `remaining` saturates, so a request already over its window reports no
  headroom rather than wrapping to an enormous one.

Six mutations, each red on a named test: treat an incomplete total as measured;
report a share of a lower bound; turn an unknown window into a zero window; wrap
instead of saturating; refuse to let a lower bound prove an overflow; omit
uncounted contributors from the explanation.

This is the fourth place the *unknown is not zero* rule is enforced
independently — C11, P12, TEL01 and now C13 — and the first where the
consequence is a number a user reads directly.

Gates: fmt clean · clippy clean under `-D warnings` · `heycode-llm` 83 lib + 266 it,
0 failed.

## PAWS02, PLM03 — two more delegated rows

`| [x] | PAWS02 | P1 | Mantle /models discovery | CAT02,PAWS01 | Exact accessible models normalized |`
`| [x] | PLM03 | P1 | Conservative tool-capable route validation | PLM02 | Chat-only model is not offered for agent mode |`

Verified: `heycode-provider-aws` 85 tests, `heycode-provider-lmstudio` 91, both
fmt- and clippy-clean.

**PAWS02** built where the escalation put it — `heycode-provider-aws` under provider
id `bedrock-mantle` — reusing PAWS03's authorizer and model id rather than
duplicating them, and widening three helpers to `pub(crate)` so the two AWS
surfaces cannot drift on what a 403 means. Every row normalizes to
`ModelDescriptor::unknown(id)`, which is the honest ceiling for an endpoint AWS
documents as reliable only in `model.id`; the wire struct has exactly one field,
so there is nothing else on it to be mistaken for evidence.

I asked for a dedicated `MantleEndpointCapabilities` type to keep endpoint
capabilities from becoming model capabilities. The lane implemented the
guarantee and showed the type was unnecessary — endpoint facts already live on
`ProviderDescriptor`, model facts on `ModelCapabilities`, and nothing converts
between them; a new type with no consumer is GOTCHAS #143's speculative rank.
`endpoint_capability_never_becomes_a_model_capability` proves it. The better
outcome, and not the one I specified.

It also reported gate 4 as **failed** rather than passed-with-an-excuse, named
the exact cause in another lane's crate, and proved it was not its own by
checking the dependency direction. `heycode-mcp` compiles now.

**PLM03** maps PLM02's tri-state to agent eligibility conservatively: only proven
support is offered. The row's risk was flattening *unproven* into *incapable*,
and the lane enforced the distinction in three independent places — separate
variants, separate messages, and an `is_published_denial()` accessor a consumer
must check before rendering "this model cannot use tools". Mutations R03 and R04
are exactly those two flattenings and each dies by a test named for the rule.
A refused model is still a usable chat model, pinned, so withholding agent mode
cannot quietly become withholding the model.

It declined to widen scope to the protocol dimension — a tool-trained model on a
server whose tool-carrying protocol was never observed is still offered — and
named the gap rather than closing it unasked. Correct: the row's dependency is
`PLM02` alone and that join is route composition above the crate.

**Evidence gap now load-bearing for three rows:** nothing in `heycode-provider-lmstudio`
has run against a real LM Studio install. If real installs commonly omit
`capabilities.trained_for_tool_use`, PLM03's conservative rule withholds agent
mode from most of a user's library and nothing would reveal it. One person with
LM Studio 0.4.x listing their library settles it.

## PGCP03 — three files assigned outside the lane's crate

The lane stopped before writing code and asked, correctly: the entire row lives
in `heycode-core/src/vocab.rs`, `heycode-session/src/request_projection.rs` and
`heycode-llm/src/gemini.rs`, with no provider-owned half. P06 left the hole
deliberately and says so in the code — the adapter validates a thought signature
on ingress and then drops it, refusing continuation on egress, because no Gemini
provider-state kind exists.

Assigned all three. Decisions recorded at the point of decision:
- `ProviderStateKind::GeminiModelContent`, naming the wire shape rather than a
  vendor concept, validated like `AnthropicMessage`.
- The forward-compat door is opened **deliberately**: a v2 log carrying an
  unknown kind hard-errors on an older reader, which is the intended closed-set
  contract (AGENTS §1), not a regression. The alternative costs the
  compiler-enforced exhaustiveness that makes provider state safe.

The lane also found a real P06 defect I verified before answering: `gemini.rs`
consumes the parsed signature only in the function-call and signature-only
branches, so a part carrying **both** `text` and `thoughtSignature` drops it.
Google documents exactly that shape — a signature on an empty text part during a
streaming response with no function call. Invisible today because nothing
consumes signatures; silent reasoning-quality loss the moment provider state
exists. Assigned under PGCP03, with the note that P06's existing signature-only
test would keep passing.

The design constraint that decides the shape, from Google's page: *"You must
return this signature in the exact part where it was received."* Durable state
must therefore be the verbatim `Content`, not a distilled signature field — a
side-channel would satisfy a test and still 400.

## PGCP03 — Gemini thought-signature state

Row: `| [x] | PGCP03 | P1 | Gemini thought-signature state | C04,P06 | Tool continuation never loses signature |`

Five files across three crates, all assigned after the lane stopped and asked:
`ProviderStateKind::GeminiModelContent` and its protocol pair in `heycode-core`,
one projection arm in `heycode-session`, and ingress/egress/resolve/chronology in
`crates/heycode-llm/src/gemini.rs`. Verified: `heycode-core` 62, `heycode-session` 97,
`heycode-llm` 358, all green; 19 mutations, no survivors in the final run.

**The P06 defect was fixed structurally rather than patched.** I had expected a
change to the `text` branch that dropped the parsed signature. The lane instead
records the raw part at the `candidate()` level, above every normalization
branch, so a signature survives on a text part, a function call or anything
added later — by construction rather than by a second code path that could rot.
Better than what I asked for.

Design decisions recorded at the point of decision:
- **Parts are never merged.** Coalescing two streamed text parts would move a
  signature off the part that owned it.
- **Thought-summary parts are replayed too**, because the documentation says to
  return what was received and filtering them would be invention. The lane named
  this as the one choice it would most want a live turn to confirm.
- **The forward-compat door is open**, per my decision: a v2 log carrying
  `gemini_model_content` hard-errors on an older reader. Intended closed-set
  contract, one-way, and it shipped today.

Three P06 tests changed with justification and no assertion weakened — including
one whose premise ("Gemini has no state kind") the row made false, renamed to
pin the C05 desync invariant instead.

**Two mutation survivors, both in its own tests, both reported.** One exposed a
*comment* claiming an ordering was protective when the parser's terminal flag
already guaranteed it — the lane kept the ordering, rewrote the comment to say
what actually guarantees the property, and recorded that the line is not
load-bearing. The other exposed a single-chunk fixture that made "publishes
nothing on failure" unobservable. GOTCHAS #172.

Still unwired: `GeminiAdapter` needs a provider advertising it before any of
this runs against a live route. That is composition, and it is mine.

## PAWS04/PAWS06 boundary — a row must not quietly eat its neighbour's scope

The PAWS04 lane found that "cache fixture" in its acceptance is unreachable
without adding `cachePoint` request emission to `heycode-llm/src/bedrock.rs` —
P07's file. `grep cachePoint` across the adapter and its tests returns nothing.

The finding underneath it matters more than the boundary question: **heycode cannot
obtain a Bedrock prompt-cache hit today.** Converse enables caching by placing
`{"cachePoint": {"type": "default"}}` blocks in `system`, `messages` and
`toolConfig`, and the adapter emits none — so P07's cache *accounting* is
correct and complete while the counters it reads will be absent forever. A cache
fixture at the profile layer would have been asserting the accounting of
something we never ask for.

Resolved by reading PAWS06's own acceptance: `Request and usage/cache facts
visible`. The **request** side is PAWS06's, so `cachePoint` emission stays
there, `heycode-llm/src/bedrock.rs` goes with that row when it is assigned, and
PAWS04 states plainly that its cache coverage is response-accounting only.
PAWS06's dependency list gained `P07` accordingly — the third tracker
correction, and the second of the understatement kind (GOTCHAS #165).

Two other decisions from the same exchange:

**Default model comes from `cfg.llm.model`.** A provider default is a versioned
cross-crate contract, AWS no longer publishes a citable list of id strings, a
geography-prefixed profile id is region-locked, and a bare foundation id only
works where that model has on-demand throughput. The lane refused to encode a
guess and made it a required config parameter; the composition root fills it
from the user's own `[llm] model`, which sidesteps region-locking entirely.

**The joining rule is the row's real content.** AWS documents checking
`responseStreamingSupported` via `GetFoundationModel`, and PAWS03 already
retains exactly that as tri-state — the field CAT02's `ModelDescriptor` has no
home for, which is why PAWS03 kept it on a provider-owned row. A model is
Converse-streamable only on explicit `Supported`; Unknown is not promoted. Two
rows joining through a field a third row deliberately preserved.

A trap worth recording: in AWS's API-compatibility matrix, the asterisk beside
model names reads like an inference-profile marker. The footnote says it means
the model also supports `InvokeModelWithResponseStream`. Read the other way it
produces a default that works in the regions you tested and fails elsewhere.

## PL05 — marketplace sources, catalog, signature and checksum

Row: `| [x] | PL05 | P1 | Marketplace sources/catalog/signature/checksum | PL02 | Pin/provenance/substitution tests pass |`

Verified: `heycode-extensions` 83 tests (48 pre-existing + 30 new + 5 unit), fmt
and clippy clean. 27 mutations, no survivors in the final run.

**The row's real content is an authority argument, not a hash check.** PL02
already rehashes a committed object against its no-clobber ref — but that is
self-consistency: on a first install the only digest in the system is the one
the cache computed from the bytes it was handed. PL05 supplies an
operator-configured publisher digest the cache does not own, and re-applies it.
The proof is one test asserting PL02's `resolve` *succeeds* on substituted bytes
in the same test where PL05 refuses them.

Three properties, each with the failure mode named:
- **Pin** by what was *requested*, never by what arrived — the latter admits any
  version the marketplace offers, a silent upgrade wearing a verification's
  clothes. Marketplace source and plugin source are independently pinned.
- **Provenance** requires host-held evidence. A package's own `[source]` block
  is retained as a claim and never feeds `origin()`; `Unknown` is terminal.
- **Substitution** goes through one function behind both `admit` and `reverify`,
  so the rule cannot hold in one direction and lapse in the other.

`SignatureState` has `Absent` and `Present` and no `Valid` — exactly as briefed.
The manifest's `source.checksum` is carried and not enforced, because it
addresses an upstream artifact rather than the canonical tree and equating them
would reject valid packages. A `Revision` pin variant was dropped rather than
recorded beside a verifiable one.

Two policy rules that are real controls: a catalog may not vend ids outside its
own namespace (a hostile catalog cannot shadow `official/foo`), and a remote
catalog may not vend a `local` package source (a document authored elsewhere
cannot point an installer at the operator's filesystem).

Findings recorded as GOTCHAS #173 and #174, including a serde defect found by
reasoning rather than mutation: `PackageOrigin` could not serialize at all, and
provenance that cannot be rendered is not reportable.

**Five documents corrected.** `AGENTS.md`, `docs/GOTCHAS.md` #112,
`MCP_AND_PLUGINS.md`, `ARCHITECTURE.md` and `THREAT_MODEL.md` all said PL05
still owed checksum *and signature* verification. Half of that is now done and
half deliberately is not, so each was rewritten to say which — the lane flagged
them rather than letting the docs quietly overclaim on its behalf.

Honest reach, now stated in the threat model: a pinned marketplace is
substitution-resistant against a hostile catalog and package server. A
marketplace able to rewrite both its catalog and the operator's pinned digest is
still outside it, because no publisher is authenticated.


## Owner process failure — a fabricated row quotation

Briefing the Q14 row I quoted a dependency and acceptance that do not exist:
`Q13` instead of the complete `B03`, and "Signature/notarization and rollback
verified on all three OSes" instead of "Fresh install and prior-version rollback
pass". Written from assumption without opening `TASKS.md`.

Caught two messages later while checking Q13's status for an unrelated reason.
Corrected to the lane in full, naming which parts of the earlier instruction to
discard rather than just saying it was wrong.

The invented acceptance would have had Q14 absorbing Q15's release-channel scope
and Q16's cross-platform matrix — the same scope-eating I had corrected in the
PAWS04/PAWS06 boundary that same afternoon. Recorded as GOTCHAS #175: a row's
title is memorable and its dependency and acceptance columns are not, so recall
reconstructs the title accurately and invents the rest with equal confidence.
That is why the standing rule is "quote it", not "know it" — and it binds the
owner writing briefs at least as tightly as a lane executing them.

## U20 — terminal capability, theme registry, persisted keymaps

Row: `| [x] | U20 | P2 | Terminal capability/theme/keymap/Vim support | U03 | Truecolor/256/dumb/narrow and persisted keymaps pass |`

Verified: `heycode-ui` 51 tests, `heycode-tui` 114, fmt and clippy clean. 38
mutations, each killed by its named test. **PL03's last blocker is now clear** —
it needed a theme registry to activate contributions into, and themes live
inside the existing `"ui"` service, so a plugin-contributed theme needs no
composition-root change.

**I gave this lane a wrong design constraint and it corrected me with sources.**
I said `NO_COLOR` set to anything, including empty, suppresses colour, and
justified it as the published convention. The published text says colour is
suppressed *"when present and not an empty string (regardless of its value)"*.
I verified it myself before accepting: the sentence is verbatim on no-color.org,
and CPython carries a fix titled "Fix `FORCE_COLOR` and `NO_COLOR` when empty
strings". The lane implemented the published rule and put it in one named
function with its citation, so overruling it is a one-line change. GOTCHAS #177.

Design decisions worth keeping:
- **Unknown → `Basic`, never higher**, with `ColorReason::Fallback` recording
  that it was not detected. `Basic` is the ECMA-48 SGR 30–37 set, below both
  tiers whose misuse garbles.
- **`NO_COLOR` does not deny a capability.** It sets the tier to `None` with
  reason `Suppressed` while `truecolor()` still reads `Supported`. A user
  preference and a terminal fact are different statements.
- **At 16 colours a theme cannot express itself**, since ANSI 0–15 have no
  defined RGB — so `Basic` uses semantic per-role slots rather than a fake
  quantization, and `Text` resolves to the terminal's own foreground so a user's
  scheme survives.
- Every capability signal is cited, including the finding that the `-256color`
  suffix is a **terminfo naming convention, not a specification** — verified
  against the local database rather than assumed.

One mutation genuinely survived and the lane fixed the **code**, not the test:
a redundant guard in `KeyChord::parse` that no test could distinguish, removed
per GOTCHAS #160, with a follow-up mutation proving the surviving check does the
work.

Its harness finding is recorded as GOTCHAS #176 and is the most useful piece of
tooling knowledge from today: a stamped mtime must outrun the **clock**, not the
previous stamp, and the nine false survivors were only visible because the
harness demanded the *named* test fail rather than accepting a non-zero exit.

Limits carried forward: keymap persistence round-trips but the live TUI still
starts from defaults (needs `keymap_plugin` in composition plus a settings
handle in the TUI — both profile-schema changes I own); `ContributionKind` has
no `Theme` variant, so a contributed theme has an effect-owned registration but
no inventory row; and Vim mode is in the row's *title* but not its acceptance,
left unbuilt because a persisted flag with no consumer is speculative — CMD10's
`/vim` is the consumer.

## Q14 scope — the lane's argument changed the crate's name and its rollback

Approved `crates/heycode-install`, renamed from the `heycode-update` I had created.
The lane's argument: `update` invites the channel and downloader logic that is
Q15's. A crate name is a scope boundary, and the wrong one recruits scope.

**Rollback is directional, not PL06's swap**, decided deliberately rather than
inherited:
- A config migration is one-directional. v1→v2 is a migration; v2→v1 is a
  refusal plus a restore-from-backup. One symmetric swap would claim an
  equivalence the code does not have — the same category of overclaim as
  `SignatureState::Valid`, moved from a signature to an operation.
- You roll a binary back because it is broken, so PL06's swap would put the
  broken version one command away. Rolling forward again is an *update to a
  retained version* — a different word for a different operation.
- PL06's loud failure carries across unchanged: a pruned retained version fails
  rather than leaving a dangling reference.

The lane also found that half the row already exists: `ConfigError::NewerSchema`
means an older binary after a rollback already fails closed, and byte-exact
migration backups keyed by the version migrated from mean the restore point is
already beside the target. So the job is to make rollback *aware* of both and
warn before the swap rather than at next start.

## PGCP04 — Gemini capability marks (accepted)

Accepted with `image_input` left **Unknown**. The lane asked whether GOTCHAS
#121 — which lets OpenRouter's catalog mark `native_web` Supported for a model
whose upstream lacks native search — transfers to marking Gemini image input
Supported from model-family naming.

It does not. OpenRouter's web search is **gateway-provided**: the route honours
it regardless of the model, so the optimistic mark is true at the layer that
makes it. Image input is **model-intrinsic**; no gateway adds it to a model that
lacks it. The two capabilities do not share an inference rule (GOTCHAS #178).

Unknown here is the tri-state working, not a hole in the row.

## MCP09 — transport wiring (behaviour change signed off)

Sibling listing walks (`resources/list`, `prompts/list`) are wired to the
connection effect. One behaviour change was surfaced by the lane rather than
taken quietly, and approved by the owner:

**A required server that advertises a listing heycode cannot walk now fails
composition**, where before only a failed *tool* walk did. `McpContributionCounts`
is documented complete and cannot express "unknown", so the alternatives were
failing loudly or publishing `0 resources` for a server that has some — the
precise false statement this stretch of work removed. Retention means an
established connection degrades rather than misreports; only the initial
connection fails, at the moment a user can act on it. The widened-`Option`
alternative was rejected: it buys robustness by reintroducing an unknown into a
field documented as complete, which is how `0 resources` reached the panel
originally (GOTCHAS #181).

Structural notes worth keeping:

- The router constructs three epochs, and **no constructor accepts one**. A
  caller cannot give two families the same watch, and cannot hold a registry
  that notifications never reach. The wrong thing is unrepresentable rather than
  merely discouraged.
- A latent bug surfaced: a runtime dropped in place on the failure path,
  panicking instead of returning an error. Unreachable since MCP02 because the
  success path only ever decremented a refcount; the sibling walks gave it a
  second trigger. The dishonest implementation (`resources: 0`, never walking)
  would never have run that path.
- **Reachability is not correctness.** The prompt catalog parses, validates and
  binds arguments and refuses a missing required argument before a request
  exists — but no service key exposes `catalog.prompts()` outside the connection
  effect. The row's acceptance is met and the product cannot yet use the result;
  both are recorded rather than resolved by moving the mark (GOTCHAS #179).

## heycode-tui — all three MCP listing families live

`McpListingSupport::CURRENT.resources` and `.prompts` flipped to `true`
(`crates/heycode-tui/src/mcp_panel.rs`). Every zero the panel renders is now a zero
heycode actually walked to. The two stale doc comments on the struct and on
`CURRENT` were corrected; the type stays because the distinction it draws —
"walked to zero" versus "never asked" — is the one the panel exists to keep, and
the next family to arrive lands `false` before it lands `true`.

Two tests had their premise invalidated and were **repurposed, not deleted**
(GOTCHAS #180). The invariant was never "these sections are unsupported" but
"an unasked zero and an answered zero must not render the same", which survives
the flip:

- `resources_and_prompts_separate_a_walked_zero_from_a_never_advertised_one`
  pins that an advertised-and-walked family renders `0` while a never-advertised
  one renders `NotAdvertised` and still never `0`.
- `an_unimplemented_listing_names_its_row_instead_of_rendering_a_count`
  constructs the unlanded support set directly, keeping the `Unsupported` arm
  exercised for the next listing family.
- `a_walked_listing_renders_the_count_it_actually_found` covers both families.

Verified by mutation: reverting the flip fails two tests; making `NotAdvertised`
render `0 <noun>` fails the distinction test. `heycode-tui` 105 integration tests,
fmt and clippy clean.

## 2026-08-29 — recovery acceptance: C08/A06 and A07 complete closure

Tracker: `C08`, `A06` and `A07` complete. Exact rollup:
**156 complete · 30 active · 106 not started · 0 blocked**. C08/A06 are
recovered-lane candidates accepted from current source and evidence rather than
from their lost reports; A07 is owner implementation on top.

### C08 crash repair

- `project_repair` is a pure read-time projection over the untouched JSONL.
  It reports open turns, steps, client calls and provider calls in log order as
  `Unknown` or `Interrupted`; no path can mint success.
- A torn final append is a reader boundary, not a repair record. `Session::open`
  returns `UnterminatedTail`, the query path treats that class as possibly
  transient, and no torn bytes reach the projection. A well-formed open record
  remains readable and reportable.
- On-disk tests cover kill/reopen, two independent crashes, idempotence/no
  write-back, partially answered parallel calls, repaired replay, shared-prefix
  forks and the torn-line classification.

### A06 ordered parallel tools

- Admission is serialized in model order. Explicit read-only calls may overlap;
  unknown and mutating calls are barriers. One cursor alternates durable/UI
  call/result commits in model order regardless of completion order.
- Exhaustive property coverage walks 4,282 combinations across batch sizes
  1..=5, every completion permutation and every parallel-safe/barrier mask. A
  fixed-seed 400-case suite adds denials and tool failures. Cancellation awaits
  admitted work and commits every unstarted declared call as an error result.

### A07 stage-complete failure ownership

- Red: the recovered request-error test was strengthened with C08's oracle. An
  unknown provider failed with one open interrupted step even though the turn
  had `turn/end error`.
- Green: `RequestErrorStage` now owns whether the step is open. Prepare/stream
  failures append `step/end` then `turn/end error`; pre-step/invariant failures
  append only the turn end. Request build/rebuild, inbox drain and tool-batch
  errors no longer escape through bare `?` after a step starts.
- `UiEvent::Error` moved after durable closure. A synchronous regression
  listener inspects the log at publication and sees the complete
  user→turn→step→step-end→turn-end sequence.
- Legacy provider selection/stream failures, strict-adapter resolve/stream
  failures and ordinary tool-error outcomes all assert C08 reports no open
  records.

Focused verification:

- `cargo fmt -p heycode-session -- --check`
- `cargo clippy -p heycode-session --all-targets -- -D warnings`
- `cargo test -p heycode-session --no-fail-fast` — **132 passed**
- `cargo fmt -p heycode-agent -- --check`
- `cargo clippy -p heycode-agent --all-targets -- -D warnings`
- `cargo test -p heycode-agent --no-fail-fast` — **135 passed**

The last full workspace gate remains the cold **2,854-test** checkpoint at
`1fc1b94`; this slice adds one source test and makes no newer full-gate claim.

Next boundary: C11 remediation. Its evidence types and arithmetic are proven,
but the audit correctly demoted it because production constructs no complete
`TokenEnvelope`; the next slice must build one from the actual request/session
planes, including provider state and attachments, before restoring `[x]`.

## 2026-08-29 — C11 restored: production five-contributor request envelopes

Tracker: `C11` complete again, after closing the audit finding rather than
reaffirming its unit tests. Exact rollup: **157 complete · 29 active · 106 not
started · 0 blocked**. Actionable implementation falls to 77; total unfinished
is 135.

The missing acceptance evidence was reachability: every envelope was
hand-assembled in tests and `ProviderState` had no construction path. That is
now closed through both actual request planes:

- `measure_resolved_call_envelope` reads a strict adapter's real
  `ResolvedCall`, including lossless provider-state items and native media.
- `measure_chat_request_envelope` reads a compatibility provider's real
  `ChatRequest`; ProviderState is exact zero because that plane does not exist.
- Both always publish System, Messages, Tools, ProviderState and Attachments in
  stable order. Message media is stripped from the countable clone and retained
  independently as `Uncounted(Unmeasurable)`, so text is not lost and bytes are
  never zeroed.

Evidence decisions:

- Transcript Messages use `TokenCounterRegistry`, so an eligible provider
  measurement outranks the local estimate.
- System, tool schemas and lossless provider state use the explicit local
  estimator. A provider endpoint cannot isolate those contributors without an
  invented transcript, and summing provider-exact subset requests would
  double-count framing while still reading as Exact.
- `EnvelopeEntry` retains every better-ranked counter refusal. A fallback is
  therefore visible instead of disappearing when `TokenCountOutcome` becomes
  a contributor.
- Counter absence/refusal/failure maps to a closed uncounted reason; caller
  cancellation returns no partial envelope. Agent wraps measurement in one
  child token, cancels and awaits it on either turn or caller cancellation, and
  publishes only the complete value.

Production composition:

- `agent` and `subagent` now inject the effect-owned `token-counters` service.
  Every child Agent inherits that exact registry; no hidden estimator is
  constructed inside execution.
- Strict requests publish the envelope only after durable request
  header/context projection and verification. Legacy calls publish before their
  compatibility stream and after durable user/step admission.
- `Agent::token_envelope()` exposes the last complete evidence object for U16;
  it never flattens an incomplete total into the existing bare status number.
- Root config schema is **20**. v19 and all older exact profiles insert
  `token-counters` exactly once before the first `agent` or `subagent` Consumer.

One adjacent correctness fix was required. Anthropic's exact count encoder
understood plain system/user/assistant text and tools, but silently ignored
assistant tool calls and media inside a message. It now refuses tool results,
assistant tool calls, images and documents, allowing the registry to fall
visibly to lower evidence rather than under-counting with an Exact label.

TDD and focused verification:

- Red: the new strict production-measurement test failed to compile because no
  producer/error API existed.
- Four LLM integration contracts pin strict five-plane construction, legacy
  media separation, cancellation with no partial envelope and visible exact→
  estimate fallback. Agent contracts prove both legacy publication and strict
  provider-state measurement; config pins v19→v20 ordering.
- Formatting and warnings-denied clippy passed across LLM, Agent, config,
  Anthropic provider, skills, status, TUI and CLI.
- All selected package tests passed: LLM **380**, Agent **135**, config **53**,
  Anthropic provider **37**, skills **13**, status **24**, TUI **129**, CLI
  **98**. Three stale exact-profile expectations failed in the first combined
  run; after adding the required provider row, only the two failed integration
  binaries were rerun and both passed.

Current source inventory is **2,860 tests**. The last full workspace milestone
remains the cold 2,854-test checkpoint at `1fc1b94`; no newer full-gate claim is
made.

## 2026-08-29 — PMM03 and PZA03 fixture-state acceptance

Tracker: `PMM03` and `PZA03` complete. Exact rollup:
**165 complete · 20 active · 107 not started · 0 blocked**; total unfinished is
127. These rows' acceptances are fixture/state contracts. Neither completion is
a claim that a composed product inference route exists.

### PMM03 MiniMax assistant state

- `MiniMaxStateRoute` is bound to product, canonical model, protocol and one of
  native `<think>`, split reasoning-details or Anthropic-compatible block
  dialects. Complete state replays byte-semantically.
- Capture/replay rejects cross-product/model/dialect state, native tags on a
  split route, duplicate Chat tool-call ids, duplicate Messages tool-use ids and
  missing reasoning where the request guarantees it.
- MiniMax's concrete official Anthropic-compatible response example includes an
  opaque `signature` on thinking blocks; the route now requires and preserves
  it, matching the shared Messages adapter rather than inferring from a shorter
  overview page.
- MiniMax-M3 omission defaults are protocol-specific; Optional never claims an
  adaptive route may omit reasoning. Unknown blocks/details remain intact.

Evidence: initial new guards red, three compiling mutations killed, state **50**
and package **116** tests plus 2 doctests green; package fmt and warnings-denied
clippy green. No MiniMax inference adapter/plugin or live state turn exists.

### PZA03 Z.AI/GLM state and model-scoped effort

- The recovered route incorrectly sent `reasoning_effort=max` to every model
  with reasoning support. Official Z.AI documentation scopes that separate
  field to GLM-5.2 and above. Canonical GLM-5.2/5.3/5.3-Flash now use the effort
  route; older models receive no invented field.
- Older compulsory-thinking models still enforce complete
  `reasoning_content` on both replay and response through provider-local guards.
  Missing state produces no successful provider state/Finish; valid state
  replays unchanged through the next tool step.
- GLM-5.3's existing two-round fixture continues to preserve reasoning blocks
  and function calls in exact order. Optional older hybrid rows accept a valid
  thinking-free tool turn while still omitting effort.

Evidence: intended red on GLM-4.6 effort, continuation-guard mutation killed,
package **63** tests plus 2 doctests, fmt and warnings-denied clippy green. The
general endpoint still cannot express `thinking.clear_thinking=false`; no Z.AI
inference plugin, authenticated live multi-step turn or Coding Plan wire claim
is made.

## 2026-08-29 — TEL05 accepted; CMD04 rejected back to not-started

Tracker: `TEL05` complete; `CMD04` returned from active to not-started. Exact
rollup: **158 complete · 27 active · 107 not started · 0 blocked**. Total
unfinished is 134.

### CMD04 acceptance audit

The recovered panel-command candidate is useful but does not satisfy its row:

- `CapabilityPanel` has only `Mcp` and `Plugins`.
- The plugin registers only `/mcp` (`COMMANDED_PANELS` deliberately excludes
  Plugins because `/plugins` remains the built-in inventory command).
- Its own module contract explicitly says `/skills`, `/agents` and `/hooks`
  have no variants because those panels do not exist.

The acceptance is “each opens its owning capability panel”, so a command that
lists inventory, a missing command, or an empty shell cannot substitute. CMD04
returned to `[ ]`; the code stays as the honest completed `/mcp` foundation.

### TEL05 retained health

The recovered `heycode-status::health` implementation was independently audited:

- schema-v1 entries contain closed check ids/status/codes/evidence kinds and
  numbers; free evidence bodies, config paths and arbitrary report text are
  structurally absent;
- `HealthLabel` screens credentials on construction and deserialization;
- every load and write enforces the 64-entry, 256-KiB, 64-check and label caps;
- count and byte caps bind in separate tests; recent unhealthy runs are
  protected preferentially but never exempt from the hard cap;
- file order survives a backwards clock; middle corruption costs one visible
  entry; an unterminated tail is reported distinctly; a later record repairs
  only the valid bounded prefix while reporting the damage;
- newer/foreign documents and directory targets are refused without overwrite;
  Unix commits are owner-only;
- a second `HealthHistoryStore` over the same path reads both prior runs,
  proving restart persistence rather than in-memory retention.

The audit found one product gap: no production factory or default profile row
mounted the otherwise complete plugin. The composition root now registers
`health-history` after the status commands, supplies
`<settings-home>/state/health-history.jsonl`, adds its service to the exact
built-in key registry, and exposes `/health`. It is an optional new default and
adds no schema migration because no historical plugin acquires an injection.

TDD and verification:

- Red: the exact default-world audit expected `health-history`; the production
  plugin list omitted it.
- `heycode-status` formatting, warnings-denied clippy and all **24 tests** passed
  (19 retained-health + 5 status commands).
- `heycode-cli` formatting/clippy passed; **97 of 98** tests passed in the focused
  run. The only failure was the expected service-key mirror; after adding the
  constitutional row and its hard-coded audit mirror, that exact failed test
  passed. No other passing target was rerun.
- Default composition now contains 80 plugins and 49 service/seam keys.

No full workspace gate was run; the prior 2,854-test milestone remains the last
workspace-wide claim.

## 2026-08-29 — K07, Q06 and X02 owner acceptance

Tracker: `K07`, `Q06` and `X02` complete. Exact rollup:
**161 complete · 24 active · 107 not started · 0 blocked**. Total unfinished is
131. PL03 remains not started: Q06 added reusable declarative substrate, not
concrete product registry activation.

### K07 real profile picker and recomposition

- Plugin `profiles` publishes `NamedProfileService` over the exact
  `NamedProfileStore` used by startup. The service is stopped by its Context
  effect, and a held handle refuses after shutdown.
- TUI contributes queued `/profile [name]`. Empty args open a sorted modal over
  the live service; the current name is highlighted; direct and modal choices
  revalidate through `load`; trust, secret, onboarding and approval surfaces
  preempt it. Queued timing prevents a profile switch from silently cancelling
  an active turn.
- Selection returns typed `RecomposeProfile`. The normal TUI teardown cancels
  and joins every outstanding operation and restores the terminal; CLI then
  removes only an existing `--profile <name>` pair, prepends the new pair,
  drops the old runtime and re-enters startup with every other original
  argument intact.
- Config schema v21 inserts `profiles` and `commands` before an existing TUI in
  historical exact profiles. Profiles without TUI are not changed.
- Production-loader evidence exercises profile service → `/profile` → typed
  UI selection. The existing real-binary named-profile test proves the selected
  layer changes composition.

Focused verification: selected-package formatting; warnings-denied clippy for
config, Agent, TUI and CLI; Agent **135**, config **55**, TUI **132** and CLI
**100** tests green. The first combined run exposed four stale schema snapshots;
the CLI's monolithic exact-inventory test then exposed three ordered mirrors one
at a time. Each rerun targeted only the failed integration binary after its
specific expected-state correction.

### Q06 terminal generation ownership

- `GenerationContext` now owns terminal `Context::shutdown()` in `Drop`.
  Retired readers may outlive a swap or the registry; the exact world remains
  usable until the last strong reader releases it, then every effect unwinds
  once.
- The lifecycle lab derives live generation/value, parked worlds, outstanding
  readers, disposal order and seam layers from each operation sequence. It
  covers clean and rejected candidates, hold/release/sweep and registry drop
  across 192 deterministic generated sequences. Removing terminal shutdown is
  killed by the shrunk zero-operation case.
- A host-neutral PL03 bridge safely freezes bounded no-follow UTF-8 package
  documents and exhaustively dispatches skill, command, agent, hook, theme and
  provider declarations through one K09 transaction. Six-kind fixtures prove
  rollback/collision/dependency/disposal, but no concrete product host consumes
  it yet; PL03 remains `[ ]`.

Focused verification from the isolated task: warnings-denied clippy; core
**62** and extensions **95** tests green. No Windows claim is made; the known
Q07 non-Unix `FileLock.path` compilation defect remains open.

### X02 effective runtime and workspace controls

- A base app-server keeps runtime/workspace capability flags false. The
  effect-owned controls generation makes them true alongside dispatchable
  `runtimes/list`, `runtime/select` and `workspace/select` methods.
- Runtime rows preserve native/delegated kind plus eight independent tri-state
  capabilities. Provider/model/runtime changes conflict with an active turn.
  Runtime selection builds a candidate, commits Routing Settings, and only then
  publishes the backend; opened or differently linked sessions reject a
  switch.
- Workspace selection requires an absolute, dot-segment-free, existing
  canonical directory contained by the composed root after symlink resolution.
  Native remains fixed to its composition workspace; delegated start/resume
  receives the selected path.
- Internal adversarial mutations killed false capability advertisement,
  outside-root admission and backend-before-settings publication. Root then
  added a real production-loader/typed-SDK test covering both capability flags,
  exact runtime listing, current-native selection and canonical native
  workspace no-op.

Focused verification: app-server **10**, SDK **4** and the added isolated
production composition test green; formatting and warnings-denied clippy green
for app-server/SDK. No external listener or IDE proof is claimed.

Accepted post-checkpoint source inventory is **2,880 tests**. The last full
workspace milestone remains the cold **2,854-test** checkpoint at `1fc1b94`;
no newer workspace-wide, live-provider or cross-platform claim is made.

## 2026-08-29 — QSEC03 host-local matrix advanced; row remains active

Tracker remains **161 complete · 24 active · 107 not started · 0 blocked**.
QSEC03 is not complete because its literal platform matrix still lacks native
Linux/Windows runs and macOS Seatbelt retains a documented hard-link alias.

Implemented host-local closures:

- Domain block policy now refuses IPv4/IPv6 literals when any block rule is
  configured, since a literal may be the translated form of a blocked name.
  An IDN requires a matching IDN allow rule; an ASCII parent rule cannot
  authorize an unlisted punycode homograph. Ordinary unrelated ASCII names and
  explicit IDNs remain reachable, while the default no-block policy retains
  public literals/IDNs.
- The same immutable policy gates registry search publication, registry fetch
  admission/final URL and every portable redirect hop. Provider output cannot
  step around it by changing spelling after dispatch.
- Seatbelt's only dynamic profile value—the workspace root—is now encoded as a
  string literal. Quotes/backslashes remain data; controls and non-UTF-8 roots
  fail closed. Structural tests count clauses outside strings, avoiding the
  prior false assertion that an escaped payload's text must disappear.
- Native macOS crafted-root, traversal, symlink, descendant, mutation and
  read-only cells pass. The matrix keeps the pre-existing hard-link write-
  through behavior visible instead of overstating path-based confinement.

The first sandbox library run happened inside another Codex task's restrictive
outer sandbox and two ordinary nested `sandbox-exec` cells failed. Direct host
probes then parsed benign, quote-escaped and backslash-escaped profiles. The
unchanged exact library target passed from the unrestricted root environment,
proving a harness-authority failure rather than a parser regression (GOTCHAS
#201).

Focused evidence: formatting and warnings-denied clippy green for web/exec/
sandbox; heycode-web **45**, heycode-exec **71**, heycode-sandbox **49**, 0 failures.
Only the initially failed sandbox library target was rerun under the corrected
environment; unrun sandbox integration and web targets then ran once. No
Linux/Windows or full-workspace claim is made.

## 2026-08-29 — MCP12 accepted: rich results cross the durable product plane

Tracker: `MCP12` complete. Exact rollup: **162 complete · 23 active · 107 not
started · 0 blocked**; total unfinished is 130. MCP15 and QSEC05 are now
dependency-ready.

The recovered candidate had a strong parser but flattened the registered Tool
result to display text. The acceptance red test called the real registered row
and observed a String: its expected schema version was JSON null. The completed
vertical is:

- `McpToolResult` retains text, image, audio, resource links, embedded text/blob,
  annotations, extension members, structured JSON, `isError` and output-schema
  evidence in exact server order. Unknown block kinds are refused. Unsupported
  schema keywords yield NotChecked, never Conforms. Explicit JSON null is
  distinct from field absence.
- `Tool::run_output` adds an optional bounded pending-rich plane while every
  ordinary Tool inherits the existing plain-JSON adapter. Pending media is
  decoded but cannot serialize or leak through Debug. Direct Tool/UI JSON
  contains content identity/length and metadata, never the bytes.
- A06's ordered commit cursor admits image/audio/blob bodies through ATT01 with
  the exact invocation cancellation token. `attachment/added` commits before
  the new v2-only `tool/rich-result`; a crash between them leaves an honestly
  open call, not a phantom result. Media admission failure becomes a durable
  tool error. Server `isError` rich results retain their blocks and error bit.
- Core owns schema-v1 `DurableToolResult`; session repair, legacy and route-aware
  projection, query, TUI replay and native runtime handle the new closed kind.
  V1 rejects it as unknown. Provider text is deterministically rendered from
  the durable blocks under the MCP data-only warning.
- Live/replay TUI and native runtime distinguish `UNTRUSTED MCP SERVER CONTENT`
  from Web. Image bytes can be reread by exact durable metadata; raw bytes,
  server text and resource URIs stay out of Debug.

Focused verification:

- formatting and warnings-denied clippy green for core, tools, session,
  attachments, MCP, Agent and TUI;
- core **64**, tools **80**, session **133**, attachments **5**, MCP **285**,
  Agent **136** and TUI **133** tests green;
- the first combined run exposed one mistaken V1 known-kind insertion, two
  stale tests that still expected flattened strings, and one unrelated process
  test whose 650-ms child timer could fire before cancellation under parallel
  load. The kind moved to V2 only, stale tests now assert typed blocks, and the
  process test uses an explicit post-cancel release sentinel (GOTCHAS #202).
  Only the failed exact targets were rerun after those changes.

No full workspace, live MCP server, Linux/Windows or prompt-injection-eval claim
is made. MCP15 owns official inspector/live OAuth evidence; QSEC05 owns the
behavioral untrusted-content evaluation.

## 2026-08-29 — TEL03 accepted: explicit OTLP without weakening local-off

Tracker: `TEL03` complete. Exact rollup: **163 complete · 22 active · 107 not
started · 0 blocked**; total unfinished is 129.

The first recovered implementation proved redaction/batching but was not
product-reachable. Owner review kept the row active and split the authority at
the crate boundary:

- `heycode-telemetry` continues to own local events, closed attributes, screened
  labels, lifecycle-owned batch worker and `telemetry-local-off`. It cannot
  depend on HTTP or credentials, so the default cannot emit structurally.
- New `heycode-telemetry-otlp` owns opt-in plugin `telemetry-otlp-http`, composed
  `HttpService`/`CredentialsService` dependencies and restart-applied,
  wire-exposed `telemetry-otlp` settings. Endpoint/resource/header/reference/
  kind/scheme are safe facts; no credential value has a settings field.
- The credential resolves for every exported batch, so rotation reaches the
  next request. OTLP/HTTP JSON uses POST `/v1/metrics`, bounded request/response,
  per-attempt deadline, capped official retryable statuses/Retry-After+jitter,
  cancellation and closed body-free faults. Partial success is refusal rather
  than silent full success.
- CLI registers the factory but does not add it to `BUILTIN_PLUGIN_ORDER`.
  A User profile must disable local-off and enable OTLP; selecting both fails
  on the duplicate `telemetry` service.
- Root's production-loader test proves OTLP service ownership, local-off
  absence, Exporting state, restart/wire settings, exact settings-namespace
  inventory and `/plugins verbose` output without recording an event or making
  a network request.

Official protocol facts were checked against the OTLP specification, the
OpenTelemetry protobuf JSON mapping and exporter configuration specification.
Focused evidence: base telemetry **116**, telemetry-OTLP **15**, one isolated
production profile test, selected formatting and warnings-denied clippy green.
No full-workspace, live collector, protobuf/gRPC/gzip or cross-platform claim is
made.

## 2026-08-29 — provider-state follow-up rollup

`PMM03` and `PZA03` were accepted after TEL03 integration; the detailed state
evidence is recorded in the earlier same-day fixture-state section. Latest exact
rollup: **165 complete · 20 active · 107 not started · 0 blocked**. MiniMax
**116** and Z.AI **63** package tests are green. Both completions remain scoped
to their literal fixture acceptances; neither provider has a composed inference
plugin or trustworthy live-turn artifact.

## 2026-08-29 — K08 restored with isolated activation diagnostics

Tracker: `K08` complete. Exact rollup: **166 complete · 19 active · 107 not
started · 0 blocked**; total unfinished is 126. The actionable implementation
bucket falls from 69 to 68. No dependency row changed state.

The audit demotion was valid: the original `doctor --composition` was
deliberately zero-apply and therefore could not satisfy an acceptance that names
activation failures. The repaired boundary preserves the safe half instead of
weakening it:

- `inspect_world` remains the public zero-apply production graph report. Invalid
  identity, dependency, service/exact collision and contribution-family state
  returns before any plugin can run.
- A healthy graph now enters a second production-loader probe in a disposable
  canonical workspace. It preserves parsed config/profile selection, redirects
  sessions/settings/credentials/attachments/catalog state under the temporary
  root, substitutes fake inference, disables watchers and resume, and clears
  configured MCP transports before factory resolution.
- `ActivationReport::diagnostic` retains only requested plugin id, scope,
  `activated|failed|not_attempted` and stable failure stage. Raw activation error
  text has no field in either human or schema-v1 JSON. The report lists every
  deliberately suppressed external action so isolation cannot masquerade as
  provider/credential/MCP evidence.
- A successful probe calls `Context::shutdown()` before the temporary root is
  removed; a failed activation already unwinds through K09's transaction owner.
  Graph failure leaves `activation: null` and exits nonzero rather than running a
  knowingly invalid selection.

TDD/evidence:

- A synthetic `Apply` failure with a secret-body canary proves human and JSON
  name the culprit/stage/later `not_attempted` row while omitting the body.
- A real production-loader probe points every ordinary product path outside its
  isolation root, enables watcher/resume inputs and supplies an MCP command that
  would create a marker. Activation is healthy, all suppressions are present,
  the marker is absent and the original product root is never created.
- Real-binary JSON proves the nested graph and activation schemas, an activated
  `tools` row, non-mutated real home paths and `activation: null` for a broken
  dependency profile.
- Warnings-denied clippy is green for core/CLI. `heycode-core` **65** and
  `heycode-cli` **102** tests pass, 0 failures. The workspace format check was not
  rerun after two concurrent provider lanes changed their files during its
  first snapshot; formatting and the full workspace gate wait for those lanes
  to settle. No newer full-gate/live-provider/platform claim is made.

## 2026-08-29 — POA02/PAN02 provider-state batch accepted

Tracker: `POA02` and `PAN02` complete. Exact rollup: **168 complete · 17
active · 107 not started · 0 blocked**; total unfinished is 124. Recovered
implementation candidates fall from eight to six and the actionable
implementation bucket from 68 to 66. No dependent row changed state.

POA02:

- `OpenAiProvider` now advertises itself as the strict inference adapter around
  the reusable Responses implementation. It selects
  `ResponsesContinuation::ReasoningAndPhase`, sends `store:false`, refuses the
  legacy chat path and validates the resolved call before transport.
- The provider-local gate refuses neutral assistant history, an assistant
  message phase outside `commentary|final_answer` and a reasoning item with
  empty encrypted content. The generic adapter still owns protocol
  serialization/state correlation; the wrapper owns OpenAI route policy.
- The acceptance fixture drives a real three-turn stateless tool loop and feeds
  the exact parser-published state into the next call. Turn three retains item
  order, encrypted reasoning, call ids and both phase values. Opening stream
  items intentionally lack encrypted content so replaying the incomplete event
  fails the fixture.

PAN02:

- `AnthropicProvider` wraps the shared Messages adapter with a provider-owned
  tool-continuation guard. Every replayed `thinking.signature` and
  `redacted_thinking.data` must remain present; every interleaved step is
  checked, and a normal user message ends the within-turn obligation.
- Adaptive mode permits text-first assistant turns and needs no beta header.
  Manual mode sends `interleaved-thinking-2025-05-14`, uses an explicit token
  budget and requires thinking/redacted thinking first.
- Manual construction no longer silently defaults to adaptive-only
  `claude-opus-5`. Root review narrowed the accompanying claim: arbitrary
  explicit ids are **not** proven compatible because the catalog has no
  complete dialect/effort/interleaving inventory.
- The stream fixture retains the exact opaque signature and original block
  order in the following HTTP request.

Current official verification:

- OpenAI's reasoning guide says stateless `all_turns` with `store:false`
  preserves every output item and explicitly calls out encrypted reasoning and
  assistant phase; its phase section defines `commentary|final_answer` and says
  manual replay keeps each original value.
- Anthropic's thinking/context documentation requires complete unmodified
  thinking blocks (including signatures) during tool use, says adaptive
  interleaving is automatic, and documents the manual-mode ordering/header and
  adaptive-only-model 400 boundaries.

Focused evidence: OpenAI formatting, warnings-denied clippy and **31** package
tests green. Anthropic formatting/clippy green; 37 package tests passed before
one mis-tagged route fixture failed, then that exact corrected test passed, so
all **38** tests are accounted without another broad rerun. No production CLI
inference registration, credentialed live provider call or full-workspace claim
is made.

## 2026-08-29 — POR06 explicit transforms reach production wire

Tracker: `POR06` complete. Exact rollup: **169 complete · 16 active · 107 not
started · 0 blocked**; total unfinished is 123 and actionable implementation is
65. POR04/POR05 remain active because no authenticated/per-call evidence was
created or inferred.

The provider-local policy was complete but unreachable. Root integration closes
the whole request path:

- `OpenRouterTransformPolicy` keeps the exhaustive known transform set,
  explicit enable/disable rows, response-healing prerequisites, requested vs
  effective evidence, user-visible effects and typed cost facts. Unknown cost
  never becomes zero; account “Prevent overrides” keeps effective execution
  Unknown.
- `OpenAiChatCompletionsConfig` now accepts multiple unique provider-option
  dialects. A dialect may project the whole durable object or one exact sole
  member. Kinds/target fields must be unique, standard Chat fields are reserved,
  and a member projection with missing or extra siblings fails before transport
  rather than dropping data.
- OpenRouter maps whole-object `routing` to top-level `provider` and unwraps
  `transforms.plugins` to top-level `plugins`. Every constructor requires the
  provider-owned transform option; there is no compatibility constructor that
  can silently omit it.
- The CLI builds `all_disabled()` for its standalone helper and credential-backed
  production plugin. The provider exposes `[routing, transforms]`; C02 persists
  both and C05 independently reprojects both before dispatch.
- Real composition with an isolated custom credential proves the production
  provider owns the full explicit disable array without making a request. The
  production-loader mock turn proves that same array reaches the wire alongside
  routing/default reasoning/native web and remains in the durable request.

TDD/evidence:

- Red: the shared Chat config had only one option mapping and no member
  projection method, so routing plus transforms did not compile.
- The focused gate first stopped on a test-root lint before tests; after the
  test-only allowance moved to that root, clippy was green. CLI **102** tests
  passed. LLM then passed 299/300 with one stale assertion still expecting one
  option; after changing that assertion to verify both kinds, only its exact
  target was rerun and passed. All LLM **383** tests are therefore accounted.
  OpenRouter **36** package tests passed.
- No live OpenRouter call was run because the host's known dummy Keychain item
  makes a local 401 untrustworthy. No full-workspace or cross-platform claim is
  made while the other provider lanes remain active.

## 2026-08-29 — PMM05 accepted; PMM04 corrected and active

Tracker: `PMM05` complete and `PMM04` active. Exact rollup: **170 complete ·
17 active · 105 not started · 0 blocked**; total unfinished is 122 and
actionable implementation is 64.

Root acceptance re-opened current MiniMax primary sources and found the
recovered PMM04 evidence classification stale: the current Token Plan MCP guide
documents both `web_search` and `understand_image`. The correction replaces
“current-only web versus legacy image” with two policy choices:

- `AllDocumented` exposes both current tools;
- `WebSearchOnly` is an explicit least-privilege restriction.

Both remain exact-allowlisted, prompt-approved and marked untrusted MCP data.
Unknown names stay denied; resources/prompts/instructions stay disabled; URL
resource delivery remains default and local delivery requires a canonical
writable directory. The bundle keeps exact `uvx minimax-coding-plan-mcp -y`
facts and a credential reference, never a value. PMM04 remains active because
the shared stdio connection owner cannot yet resolve credential-backed MCP
environment values; no real definition/connection/tool generation exists in a
product world.

PMM05 satisfies its own independent acceptance. `MiniMaxCodingProfile` accepts
only affirmative `TokenPlanSeat|PurchasedCredits`, refuses Unknown/NoResources
before returning an endpoint or credential, retains the existing Token Plan
identity/kind/reference, permits only currently documented region/protocol
routes and reports the former dedicated Coding Plan endpoint as Unknown. This
matches current MiniMax guidance that Token Plan replaces/extends Coding Plan,
that its key may exist before usable resources, and that current tool routes are
ordinary `/v1` and `/anthropic` endpoints.

TDD/evidence: the new current-doc policy names failed against the recovered enum
before implementation. After correction, package formatting and warnings-denied
clippy are green; MiniMax **125** tests and four doctest cases pass. No provider
inference activation, MCP child, live MiniMax credential, full-workspace or
cross-platform claim is made.

## 2026-08-29 — PZA04 durable native web accepted; PZA05 active

Tracker: `PZA04` complete and `PZA05` active. Exact rollup: **171 complete ·
18 active · 103 not started · 0 blocked**; total unfinished is 121 and
actionable implementation is 63.

The recovered PZA04 provider type retained all seven documented Z.AI search
fields, but its normalized events dropped site/icon/reference/publication. Root
closed that exact product gap without inventing a provider-specific session
kind:

- core `ServerToolSource` gains optional `ServerToolWebMetadata` with bounded
  site name, public icon URL, provider reference and publication string;
  existing rows deserialize with it absent and Debug reveals presence only;
- `ZaiWebSearchRecord::project` attaches the complete metadata to each durable
  result source while citations keep the existing URL/title/summary plane;
- a real `Session::append`/`Session::open` test proves the metadata survives the
  production JSONL reader and request projection;
- non-default composition factory `native-zai` maps the provider-owned identity
  into N01. A User profile proves `zai` selects it, `deepseek` does not, exact
  inventory attributes it to the plugin and shutdown disposes it.

PZA05 remains active. The provider crate validates all four official Coding
Plan servers, exact remote/local transport facts, credential references, tool
sets and transactional prefix rollback. Shared MCP connection code still
refuses credential-backed stdio environment and Streamable HTTP headers pending
an authorization provider, so registering metadata cannot honestly be called a
working bundle.

TDD/evidence: the PZA04 test first failed because `ServerToolSource` had no web
metadata accessor. The focused gate then stopped before tests when the test
tried to deserialize `SessionEvent` directly; it was corrected to use the
production Session reader. A later CLI baseline failure was unrelated but real:
the concurrently completed LM Studio lane added service
`lmstudio/model-control`. Root added its service-key registry/baseline row and
reran only that exact test. Warnings-denied clippy is green; core **65**, session
**133**, Z.AI **68** plus two doctests and CLI **103** tests are accounted green.
No live Z.AI/MCP call, full-workspace or cross-platform claim is made.

## 2026-08-29 — PLM04/PLM05 provider boundaries active

Tracker: `PLM04` and `PLM05` move from not-started to active. Exact rollup:
**171 complete · 20 active · 101 not started · 0 blocked**; total unfinished and
actionability buckets are unchanged.

Provider-local implementation:

- default `provider-lmstudio` now publishes `LmStudioModelControl` under new
  service `lmstudio/model-control`. Load planning performs no I/O and produces a
  consuming plan; execution uses native `/api/v1/models/load`, requests echoed
  config and verifies every explicit context length, eval batch, Flash
  Attention, MoE expert and GPU KV-cache choice. Unload targets an exact
  instance id; cancellation/error mismatches produce no receipt;
  `require_loaded` refuses downloaded-only models.
- Ollama is a sibling identity, not an LM Studio toggle. Its native inspector
  checks `/api/version` and `/api/tags`; its native catalog declares protocol
  Unknown. A separate OpenAI-compatibility profile/adapter checks `/v1/models`
  and a scripted `/v1/chat/completions` response under provider id `ollama`.

Neither row is accepted complete. PLM04 depends on U14 and still lacks
Settings/CAS, command/UI Consumers, post-operation catalog refresh, inference
route enforcement and explicit handling of LM Studio's global JIT setting.
PLM05 lacks production factory/picker wiring and a real installed Ollama chat
smoke. Provider package formatting/clippy are green and **104** tests pass.

The concurrently added service caused PZA04's exact default-composition test to
fail. Root added the owner constant to `BUILTIN_SERVICE_KEYS`, the constitution
service table and exact service baseline, then reran only that failed target.
This was an inventory update, not a PZA04 regression. No full-workspace/live
local-provider/platform claim is made.

## 2026-08-29 — AWS/Google provider lower layers advanced; rows remain active

Tracker moves `PAWS05`, `PAWS06`, `PGCP06` and `PGCP07` from not-started to
active. Exact rollup: **171 complete · 24 active · 97 not started · 0 blocked**;
total unfinished/actionability buckets do not change. Existing `PAWS04` and
`PGCP05` remain active.

AWS provider-local work:

- PAWS04's Converse fixture covers streaming text/tool state and cache
  accounting while retaining the hosted-smoke gap.
- PAWS05 adds distinct Mantle Responses and Messages providers with exact
  endpoint/auth/protocol capability differences and pre-transport Messages
  structured-output refusal.
- PAWS06 validates cache checkpoints/TTL, guardrail configuration and
  foundation/system cross-Region/application target classes in a body-free
  provider option.

Google provider-local work:

- PGCP05 projects grounding calls/results/citations under safer live gating.
- PGCP06 retains correlated code-execution events, provider-native candidate,
  explicit/implicit cache provenance and cached-token modality usage.
- PGCP07 defines a Sonnet 5 Google Cloud profile, exact Vertex body rewrite,
  conservative ADC preflight and an evidence probe that can become live only
  after authenticated model/tool/thinking observations.

No row completes because the shared/product halves are absent. Bedrock needs a
model-aware option hook/wire mapping, detailed durable cache usage, model
streaming eligibility join and factories. Gemini rejects provider options;
Messages hardcodes endpoint/body/header; Google token activation/factories are
absent. PAWS04/PGCP07 also explicitly require hosted/live success. Provider
formatting and warnings-denied clippy are green; AWS **117** and Google **100**
tests pass. No ambient credential was read and no live cloud request, full
workspace or cross-platform claim is made.

## 2026-08-29 — PDS05 optional capabilities accepted

Tracker: `PDS05` complete; `PDS04` remains active. Exact rollup: **172 complete
· 24 active · 96 not started · 0 blocked**; total unfinished is 120 and
actionable implementation is 62.

PDS05 has four deliberately separate provider-owned contracts:

- strict tools use beta Chat and project `strict:true` into every existing
  durable function schema; an empty tool set is refused;
- JSON Output uses standard Chat, exact `json_object` response format and the
  documented prompt-keyword admission;
- chat prefix uses beta Chat, keeps prefix text in the logged assistant message
  and carries only a bounded activation marker in provider options;
- FIM uses beta `/completions`, is non-thinking, caps output at 4096 and admits
  only the model for which the endpoint schema is explicit. Conflicting Flash
  table evidence remains Unknown.

Root rechecked current official strict-tool, JSON Output, prefix, FIM endpoint
and pricing sources. Three compile-valid mutations—disabling strictness,
rejecting exactly 4096 FIM tokens and promoting contradictory Flash evidence—
were killed. Formatting and warnings-denied clippy are green; all DeepSeek
**70** tests pass. PDS04 is not completed because its acceptance explicitly
requires an authenticated Anthropic-format smoke and no key/live observation
exists. No full-workspace or platform claim is made.

## 2026-08-29 — POA03/PAN03 provider-native tool layers active

Tracker moves `POA03` and `PAN03` from not-started to active. Exact rollup:
**172 complete · 26 active · 94 not started · 0 blocked**; total unfinished and
actionability buckets are unchanged.

OpenAI provider-local work defines seven hosted families—web/file search, code
interpreter, shell, computer use, image generation and remote MCP—with exact
request shapes, per-model capability gates, lossless completed-item classifiers,
credential-free MCP metadata and body-free errors. OpenAI **36** tests pass.

Anthropic provider-local work defines six server families—search, fetch, code,
advisor, tool search and MCP connector—with exact beta/request extensions,
tri-state model gates, lossless call/result classification and pending
`pause_turn` preservation. Current source review corrected tool-search support
for Opus 5 while keeping Opus 4.1-and-earlier Unsupported. Anthropic **43** tests
are accounted green; only diagnosed exact targets were rerun after premise/source
corrections.

Neither row completes because shared Responses/Messages parsers do not emit the
normalized N02 events, session/Agent/UI do not carry the new families, and no
production provider factory selects them. No live provider/network, full
workspace or cross-platform claim is made.

## 2026-08-29 — U14 settings browser accepted

Tracker: `U14` moves from not-started to complete. Exact rollup:
**173 complete · 26 active · 93 not started · 0 blocked**. Total unfinished is
**119**. Actionable implementation remains **62** because U14 leaves that bucket
while simultaneously unlocking `CMD10`; dependency-blocked falls to **38**.

Product path:

- plugin `ui` now publishes effect-owned `SettingsUiRegistry` under exact
  service `settings-ui`. Custom namespace ownership stores an opaque token and
  disposes back to schema derivation on Context shutdown;
- TUI contributes immediate `/settings`, shares its request inbox with the
  production `TuiHandle`, and makes the command available only after both
  `settings` and `settings-ui` are attached;
- the browser opens fresh snapshots for every registered namespace, shows
  origin and live/restart application, and commits toggle, choice, text and
  number edits through `replace_user` with the exact snapshot revision;
- stale CAS conflicts remain recoverable and reload authoritative rows.
  Managed/project/secret/unrenderable/custom rows stay visible with reasons;
  secret variants contain only a configured bit and can never carry a value;
- the production composition harness follows `/settings` through the shared
  panel request into real routing/web schema rows. Exact service and command
  inventories include `settings-ui` and `/settings`.

TDD/evidence: schema completeness, valueless secrets, all editable control
classes, stale conflict, custom-surface lifecycle and real-composition
reachability were added before acceptance. `cargo clippy -p heycode-ui -p heycode-tui
-p heycode-cli --all-targets -- -D warnings` passes. All **51 heycode-ui**, **136
heycode-tui** and **104 heycode-cli** tests pass. The first CLI gate found one exact
inventory mismatch because accepted concurrent work had added
`lmstudio/model-control`; root updated both missing service rows and reran only
that failed target. No live provider, full-workspace or cross-platform claim is
made; the last accepted full gate remains 2,854 tests.

PLM04 remains active. U14 supplies its generic settings/CAS/UI substrate, but a
provider-specific namespace and explicit load/unload Consumer, post-operation
catalog refresh, route enforcement and global-JIT control still have to land
before “no surprise load” is a product guarantee.

## 2026-08-29 — PLM04 explicit LM Studio control accepted

Tracker: `PLM04` moves from active to complete. Exact rollup:
**174 complete · 25 active · 93 not started · 0 blocked**. Total unfinished is
**118** and actionable implementation falls to **61**; no row depends on PLM04,
so no blocked bucket changes.

Product path:

- default plugin `lmstudio-control` consumes the provider's raw
  `lmstudio/model-control`, native `lmstudio/models`, shared catalogs, Settings
  and commands. It owns live namespace `lmstudio-load` and queued
  `/lmstudio <load|unload> <target>`;
- Settings express each numeric override as an explicit mode plus bounded
  value, and both hardware booleans as server-default/enabled/disabled. A
  server default is omitted from the native request rather than serialized as
  an invented value;
- load rereads the latest snapshot, admits only a downloaded affirmative
  tool-capable chat row, refuses duplicate in-memory state, consumes the raw
  one-shot plan, verifies every echoed setting, confirms the exact instance in
  a fresh native list and force-refreshes the shared picker generation;
- unload starts only from an observed exact instance id, verifies the echoed
  id, confirms removal and performs the same shared refresh;
- provider mutation, readback and shared publication have distinct error
  classes. “Accepted but readback unavailable/mismatched” and “succeeded but
  catalog refresh failed” cannot be mistaken for a clean pre-mutation failure;
- one Context lifecycle token parents operation waits and is cancelled before
  command/namespace effects unwind.

Current LM Studio primary docs confirm native load/unload/echo fields and the
global JIT behavior, but expose JIT only as a Server Settings switch—not a
stable REST/CLI mutation. heycode therefore refuses the tempting private-config
edit and guarantees only what it owns: browsing/planning perform no I/O and
only the explicit command invokes the load endpoint. A later composed local
inference path must consume the already-loaded guard; no inference or installed
LM Studio smoke is claimed here (GOTCHAS #218).

Evidence: warnings-denied all-target clippy passes for LM Studio and CLI. All
**108 heycode-provider-lmstudio** and **105 heycode-cli** tests pass. Deterministic
sequences cover exact Settings-to-body projection, readback, forced catalog
revision, exact-instance unload, effect disposal, production factory/order,
inventory, TUI settings reachability and syntax refusal before local HTTP. No
full-workspace, real-install, provider inference or cross-platform claim is
made; the last full gate remains 2,854 tests.

## 2026-08-29 — S13 competitor metadata preview accepted

Tracker: `S13` moves from not-started to complete. Exact rollup:
**175 complete · 25 active · 92 not started · 0 blocked**. Total unfinished is
**117**. S13 leaves actionable implementation while newly dependency-ready
`MCP14` enters it, so actionable stays **61** and dependency-blocked falls to
**37**.

`heycode-config::preview_competitor_config` is deliberately pure. It accepts
already-read text plus exact Codex/Claude/OpenCode format and a host-supplied
user/project/executable authority; it discovers no path and writes nothing.
Provider/model/credential-free base URL, secret-free MCP transports and a small
typed settings set retain values. Credential, environment, header, OAuth,
helper and hook fields become exclusions with path+reason only. Unknown fields
carry path+structural kind only, so an unknown secret-shaped value cannot leak
through Debug or a future renderer.

Detached candidate construction clones a typed Config, clears imported-route
credential pointers, refuses MCP name collisions, and rejects any enabled row
that would need omitted metadata or authority. Disabled rows stay visible but
unapplied because current Config has no disabled MCP state. Claude settings stay
strict JSON, OpenCode alone receives bounded JSONC normalization, and Codex
uses TOML. Current official Codex configuration, Claude settings/MCP and
OpenCode config/MCP references were rechecked.

Evidence: crate-scoped formatting and warnings-denied clippy pass; all **59
heycode-config tests** pass, including four adversarial import cases. Secret
canaries cover environment, headers, OAuth/helper/hook fields, unsafe URLs and
unknown values; project trust, executable authority, incomplete MCP rows,
collisions and unchanged source bytes are pinned. No full-workspace claim is
made. Source discovery, UI confirmation, source CAS, atomic destination writes
and unresolved auth references remain MCP14/later rather than being inferred
from the preview boundary (GOTCHAS #219).

## 2026-08-29 — E06/E08 retained output and LSP accepted

Tracker: `E06` and `E08` move from not-started to complete. Exact rollup:
**177 complete · 25 active · 90 not started · 0 blocked**. Total unfinished is
**115** and actionable implementation falls to **59**. E08 makes `X07`
dependency-ready, moving one row from dependency-blocked (**36**) to
post-beta/optional (**6**).

E06 product path:

- Unix default `retained-output-local` owns a unique 0700 generation beneath
  an isolated product root. Objects are 0600, regular, single-link SHA-256
  content; logical owner plus id is required for every lookup;
- object/entry/generation/read caps are independent, and each read revalidates
  path/open identity, mode, link count, length and the complete digest;
- the service constructs the entire preview envelope under one cap—wrapper,
  id, total bytes, escaped body and footer. The LSP Consumer returns that string
  unchanged. Shutdown removes only its generation; non-Unix stays unsupported.

E08 product path:

- default `lsp-registry` injects the existing filesystem and raw subprocess
  services. Exact `lsp-stdio` definitions are effects; lazy sessions use
  bounded Content-Length JSON-RPC, canonical workspace-confined file URIs and
  the common sandbox/process-tree owner;
- caller cancellation sends `$/cancelRequest` and cancels only that read. The
  connection remains reusable; definition/registry teardown cancels the
  session and reaps descendants;
- `LspBackend` now has constructible safe descriptors/locations/diagnostics and
  fixed error codes, so replacement Providers can implement the public trait;
- Unix default `lsp-tools` contributes `lsp_servers`, `lsp_definition`,
  `lsp_references` and `lsp_diagnostics`. A zero-definition world returns `[]`
  and starts nothing. Results above 64 KiB spill through E06;
- Core adds `UntrustedContentSource::Lsp`. Agent/session/native runtime/TUI
  preserve and label language-server output separately from Web and MCP.

Evidence: warnings-denied clippy and focused tests are green at **Core 66, Exec
81, Tools 81, Agent 159, TUI 137 and CLI 106**. Real stdio fixtures cover
definition/references/document diagnostics, cancel reuse and descendant death;
tool tests cover discovery, mapping and large spill; real composition pins both
services, all four tools and no process for an empty registry. The initial CLI
run found only a lexicographic expected-service ordering mistake (`lmstudio*`
sorts before `lsp`); root corrected it and reran that exact target. No real
language server, trusted project definition, IDE, non-Unix retained backend,
full-workspace or cross-platform claim is made (GOTCHAS #220).

## 2026-08-29 — A08/A09 deferred tools and durable loop budgets accepted

Tracker: `A08` and `A09` move from not-started to complete. Exact rollup:
**179 complete · 25 active · 88 not started · 0 blocked**. Total unfinished is
**113** and actionable implementation falls to **57**; no downstream row changes
bucket.

A08 product path:

- Agent-owned `DeferredToolCatalog` joins current client schemas and N01 routes;
  a provider selection filters those two planes plus prompt tool-name context as
  one request-preparation transaction;
- default effect plugin `deferred-tools` installs
  `LexicalDeferredToolProvider(max_selected=64)`. Current small catalogs pass
  unchanged. Only larger catalogs score query overlap, use registry order as a
  stable tie-breaker and retain at most 64 rows; cancellation/failure does not
  restore the full catalog;
- `CodeModeSchedule` emits only ordinary selected legacy chunks or strict
  inference events. It imports neither ToolRegistry nor execution helpers, so
  A06 remains the sole approval/execution/cancellation/ordered-commit owner;
- the 5,000-tool fixture measures exact serialized schema bytes and JSON nodes,
  avoiding a machine-noise wall-clock claim while proving >1000× reduction.

A09 product path:

- default `loop-budget-settings` registers restart-applied namespace
  `loop-budget`: explicit 64 steps, 1,000,000 reported-token lower bound, one
  hour, 256 client calls and `allow-lower-bound`; deployments may select strict
  `require-reported`;
- one effect-owned pre-step layer reconstructs completed steps, reported usage,
  unreported steps, dispatched calls and elapsed time from JSONL before every
  request. It owns no mutable counter and resumed sessions make the same next
  decision;
- session `TurnEndReason` now owns exact `max_steps`, `max_elapsed`,
  `max_tool_calls`, `unreported_token_usage` and `clock_unavailable`, while
  `max_tokens` remains exact. Native runtime maps all budget stops to `Limit`;
- the plugin marker suppresses the legacy eight-pause fallback only for its
  effect lifetime. Exact profiles omitting it retain the compatibility guard.

Evidence: formatting and warnings-denied clippy are clean; **162 Agent**, **133
Session** and **107 CLI** tests pass. Real composition exercises both default
plugins on a turn and verifies the Settings namespace/layer/metrics. One stale
test expected generic `error` after the exact max-step reason landed; root
updated it and reran only that target. No provider-specific Code Mode opaque
state, full-workspace or platform claim is made (GOTCHAS #221).

## 2026-08-29 — PLM05 product bridge complete; live smoke remains

Tracker row stays active and the exact rollup remains **179 complete · 25
active · 88 not started · 0 blocked**. Its remaining work changes class:
active external evidence rises to **8** and actionable implementation falls to
**56**. Total unfinished remains **113**.

Provider-local `provider-ollama` now owns four distinct services: joined
catalog, explicit profile, concrete Chat inference and read-only inspector. Its
picker generation requires agreement across `/api/version`, `/api/tags`,
`/api/ps`, per-model `/api/show` and `/v1/models`; only completion-capable rows
survive, while tools/vision/thinking and context/running metadata remain exact.
Native tags alone still report protocol Unknown. No LM Studio type, endpoint,
setting or control path is reused.

Root product integration is conditional rather than auto-discovery:

- `[llm] provider="ollama"` plus an explicit model registers
  `provider-ollama` after `models`; optional `llm.base_url` is validated, while
  omission uses the documented localhost origin;
- the `llm` plugin injects the published `OllamaInference`, registers it under
  the shared provider registry and publishes the exact model selection;
- startup bypasses credential presence/validation only for Ollama. Any
  `api_key_env` fails before lookup and the provider exposes no credential
  reference. The compatibility wire key is a protocol constant, not secret
  state;
- composition performs no HTTP, process, daemon start, model pull or model
  choice. Catalog/model picker refresh remains explicit runtime I/O.

Evidence: provider and CLI warnings-denied gates pass with **110 tests each**.
Real composition proves provider/profile/catalog descriptor agreement and the
credential-free picker bridge; deterministic provider fixtures prove the exact
read-only joins and mock Chat wire. The generated capability reference now
lists Ollama as an inference route. `command -v ollama` returned empty, so the
acceptance's real installed-model chat smoke cannot run honestly. PLM05 remains
active for that external evidence and a future fresh setup picker that discovers
an available model without guessing (GOTCHAS #222).

## 2026-08-29 — MCP14 non-secret competitor import accepted

Tracker: `MCP14` moves from not-started to complete. Exact rollup:
**180 complete · 25 active · 87 not started · 0 blocked**; total unfinished is
**112**. MCP14 leaves actionable implementation, while the audit corrects C12
from external-only back to implementation because native compaction is missing:
external evidence becomes **7**, actionable implementation remains **56**.

`preview_competitor_mcp_import` consumes only S13's value-minimized public
preview. It never sees source bytes/files, credential values, header/environment
maps or unrelated provider/settings/unknown values. Clean rows become exact
private reconnect-disabled definitions. Excluded credential paths become
deterministic unresolved `McpSecretReference` requests carrying only source
field path and binding role; header/environment names and values are not
reconstructed.

Whole-set extraction preserves project trust and executable authority without
an override knob. Enabled incomplete rows, invalid absolute cwd/transport/id
and existing-name conflicts return no candidate vector. Disabled incomplete
rows stay visible with unresolved references and activate nothing. Changing
only secret values produces an equal preview, which makes the absence of secret
influence observable.

Evidence: the four exact MCP14 tests pass and crate fmt/warnings-denied clippy
are green. The broader package run reached 8 unit + 289 integration tests; three
pre-existing Streamable HTTP real-socket tests failed only because the separate
task sandbox forbids `TcpListener::bind`. They are recorded as environment-
blocked, not MCP14 failures and not passes; no unavailable escalation or repeat
bind was used. No credential binding, UI discovery, persistence or live MCP
server is claimed (GOTCHAS #223).

## 2026-08-29 — CAT06 provenance gap closed; CAT07 library accepted, product open

Tracker: `CAT06` moves from active back to complete. Exact rollup:
**181 complete · 24 active · 87 not started · 0 blocked**; total unfinished is
**111** and actionable implementation falls to **55**. CAT07 remains active but
is no longer an unreviewed recovery candidate.

CAT06's demotion finding was exact: generation provenance could not travel with
a detached pricing/performance value. `ModelMetadataProvenance` now validates a
safe source label plus nonzero capture instant, and every non-empty
`ModelPricing`/`ModelPerformance` constructor requires it. Unknown values have
none. Currency/unit/component/amount invariants remain exact and advisory facts
still cannot affect capability or request resolution.

The first provider review found a real migration bug before acceptance:
OpenRouter production code still built `ModelPricing::unknown().with(...)`,
which the new invariant correctly rejects. Root split parse from construction,
captures one `openrouter:models-api` instant after the full/detail join, and
starts each non-empty price with `captured`. One malformed price still rejects
the generation; empty pricing remains Unknown.

Catalog cache schema v2 stores provenance inside pricing/performance. Schema v1
is still readable, but its source-less advisory facts restore as Unknown rather
than borrowing the enclosing generation time. Current non-empty metadata
without provenance fails loud.

CAT07's reviewed library keeps override files separate from the cache. The
document cannot spell provenance; the loader mints layer/path sources. Per-field
precedence, assertion direction, contradictions and unmatched models are
visible in `AttributedCatalog`; assertions never mutate/add provider rows or
persist into cache. All 18 adversarial override tests pass. Root then added
default `catalog-overrides` over user plus trust-gated project files and passed
the optional service into TUI. Picker rows now label assertions, contradictions
and unmatched models; raw provider capability filters never admit a user claim.
The tracker remains active because those assertions are still preview-only:
durable request context/C05 does not record or enforce their attribution.

Evidence: formatting and warnings-denied clippy pass; **384 LLM**, **26
catalog-file** and **36 OpenRouter** tests pass. The added composed/TUI gate is
green at **138 TUI** and **111 CLI** tests after correcting one test premise
(narrowing support is not a contradiction; claiming support against a provider
denial is). No live provider, enforced override, full-workspace or platform
claim is made (GOTCHAS #224).

## 2026-08-29 — C12 compaction kernel accepted end to end

Tracker: `C12` moves from active to complete. Exact rollup:
**182 complete · 23 active · 87 not started · 0 blocked**; total unfinished is
**110**. Completing the row removes one actionable implementation item and
unlocks POA04, PAN04, PAN05 and C14 as implementation plus C15 as stress proof:
the scheduling buckets become **7 external · 58 implementation · 8 proof/docs ·
31 dependency-blocked · 6 optional**.

C12 now has one production path rather than three bypasses:

- default plugin/service `compactions` contributes effect-owned exact rows
  `provider-native`, `portable-summary` and `prune-oldest` before Agent and
  subagent Consumers; schema v22 inserts it before the first such Consumer in
  historical exact profiles;
- a `CompactionStrategy` prepares a read-only `CompactionPlan`. The registry
  snapshots the log, rejects any strategy/concurrent durable mutation, verifies
  the descriptor matches summary versus provider-state replacement and alone
  appends one settlement. Cancellation/no-op/failure append nothing;
- `/compact`, automatic pressure middleware and native RuntimeSession
  compaction all resolve `portable-summary` through that registry. Portable
  summary supports both legacy Chat and strict `InferenceAdapter` dispatch with
  exact `CallPurpose::Compaction`;
- `heycode-llm` adds optional provider transport
  `InferenceAdapter::native_compaction`. Capability evidence and operation
  availability are independent; a resolved compaction call returns a bounded
  `NativeCompactionCheckpoint` whose items share provider/model/protocol. The
  Agent owns cancellation settlement and the durable append;
- session v2 adds closed `compaction/native { strategy, replaced_upto_seq,
  items, usage? }`. Exact-route projection injects the checkpoint and shadows
  its prefix; neutral/incompatible routes ignore the marker and retain original
  history. V1 rejects the kind. Append/open/C05 projection reject a marker that
  names itself or a future sequence;
- prune and portable/native folds share the same balanced keep boundary,
  including the leading user message and attachment selection.

Focused evidence is green after diagnosed, targeted corrections:

- `cargo check` across LLM/session/Agent/config/CLI all targets passes;
- warnings-denied clippy passes across LLM, session, Agent, config, CLI, skills,
  status and TUI;
- accounted tests are **385 LLM, 137 session, 163 Agent, 60 config, 112 CLI,
  13 skills, 24 status and 138 TUI**. The first no-fail-fast run found only one
  empty transaction fixture and three exact expected-profile arrays; root
  seeded the former, added the required v22 row to the latter and reran only
  the failed Agent/CLI targets plus the 49-test config migration target. No
  unchanged failing command was looped;
- real default composition proves all three rows and their shutdown disposal;
  a strict fake adapter proves resolved native dispatch, one durable checkpoint
  and exact-route continuation. No OpenAI/Anthropic live/provider activation,
  full-workspace or platform claim is made.

Default Unix composition is now **89 plugins**, the service registry **60
keys**, config schema **22**, and the hard-won index **225** (GOTCHAS #145/#225).

## 2026-08-29 — C15 durable compaction stress accepted

Tracker: `C15` moves from not-started to complete. Exact rollup:
**183 complete · 23 active · 86 not started · 0 blocked**; unfinished becomes
**109**. Actionable proof/docs returns from 8 to **7**; the other buckets remain
7 external, 58 implementation, 31 dependency-blocked and 6 optional.

The new session integration fixture writes a real **1,000-turn, 9,011-event**
JSONL stream. Every turn carries user/turn/step, correlated request header and
context, one exact OpenAI Responses provider item, one neutral assistant copy
and complete settlement. Every hundred turns it folds to a ten-turn keep window,
alternating portable and route-scoped native checkpoints; the final settlement
is native.

After dropping and reopening the Session, the test proves:

- every logical sequence equals its event index;
- the production `project_requests` path validates/reconstructs all 1,000
  correlated requests;
- exact OpenAI projection begins with the final `cmp_999` checkpoint and retains
  every `msg_990`…`msg_999` provider item;
- Anthropic projection contains only neutral messages, retains the latest human
  question and contains no OpenAI opaque state.

The exact target passed once in **0.66 s**, but no wall-clock assertion or
performance claim was added; C15 is structural stress and Q22 owns performance
budgets. Session focused accounting is now **138 tests**, raw source inventory
**3,118**, and GOTCHAS #226 records why a long append loop alone would not have
accepted the row.

## 2026-08-30 — C14 opaque-state route-switch policy accepted

Tracker: `C14` moves from not-started to complete. Exact rollup at acceptance:
**184 complete · 23 active · 85 not started · 0 blocked**; unfinished becomes
**108** and actionable implementation falls from 58 to **57**.

The session owner now projects one safe `OpaqueCompactionBarrier` from the
winning settlement, using later-wins ties so a portable recompact covering the
same prefix can supersede native state. The barrier retains provider/model/
protocol, replaced boundary and `EventCount(native_settlement.seq)`, which is
the exact prefix immediately before the marker.

Agent and routing execute three distinct effects:

- direct provider/model selection, initial Settings apply and external watcher
  publication all refuse the barrier before live route mutation;
- `portable` runs the current provider's `portable-summary` with a one-turn keep,
  rechecks that the barrier cleared, then commits Settings/live selection;
- `fork` creates a shared-prefix child before native settlement and leaves the
  parent/route unchanged; `cancel` changes nothing.

Human commands expose the second argument on both `/provider` and `/model`.
App-server v1 cannot express the choice and maps it to Conflict. The first Agent
test exposed a real fold bug: code treated any event after `turn/end` as an open
turn, so a native marker made portable resolution a no-op. Fold candidates now
match each start to its own later end by turn id (GOTCHAS #227).

Evidence: exact session barrier, Agent three-option and production CLI routing
targets pass; warnings-denied clippy is green for session/Agent/routing/
app-server/CLI and shared format/diff checks are clean. Session **139**, Agent
**164** and CLI **113** were the accounted focused counts before the provider
activation tests below. No full-workspace or live-provider claim is made.

## 2026-08-30 — POA04/PAN04 production native compaction accepted; cache/edit rows active

Tracker changes: `POA04` and `PAN04` move from not-started to complete;
`POA05`, `PAN05` and `PAN06` move from not-started to active at named shared
gaps. Exact current rollup: **186 complete · 26 active · 80 not started · 0
blocked**; unfinished is **106**. Actionable implementation falls from 57 to
**55**; other buckets remain 7 external, 7 proof/docs, 31 dependency-blocked
and 6 optional.

Provider and root product bridge:

- OpenAI's strict provider exposes a distinct buffered
  `POST /v1/responses/compact` operation for exact evidenced `gpt-5.6-sol`,
  preserves every returned compacted user/opaque item and extension unchanged,
  normalizes input/output totals and maps cancellation/status/transport/body
  failures to closed C12 classes. The exact checkpoint continues through a
  later `store:false` Responses request;
- Anthropic's strict provider injects the current compaction beta/edit into one
  no-retry Messages operation, normalizes the provider compaction stop to Pause,
  retains the complete assistant item (thinking/signatures/text/tools/unknown
  extensions) and returns one C12 checkpoint after terminal settlement;
- `openai` and `anthropic` move from configured-only to production inference
  provider ids. The credential-backed `llm` factory constructs each over the
  composed HTTP service. Real-composition tests disable the normal fake, seed a
  unique owner-only temporary credential reference and inspect strict/native
  interfaces without making a request. Portable compaction remains default.

At this checkpoint POA05/PAN05/PAN06 remained active rather than overclaimed. Provider-local prompt
cache controls, cache read/write/reasoning counters, ordered context edits,
applied-edit/cache-impact metadata and structured count-token bodies now exist,
but neutral durable events/usage projection, settings producers and usage UI do
not. Anthropic's current guide calls `/count_tokens` an estimate, so root added
`Estimated(ProviderTokenizer)`: it outranks the local UTF-8 ratio but can never
be read as Exact (GOTCHAS #228).

Evidence: provider gates are **45 OpenAI** and **55 Anthropic** tests, both with
warnings denied; the shared LLM token-counting family passes **21** targets;
production OpenAI/Anthropic composition and C14 exact targets pass. Current CLI
accounting is **115**, raw source inventory **3,132**. No live credentialed call,
full-workspace or platform claim is made.

## 2026-08-30 — DOC03 generated capability reference accepted

Tracker: `DOC03` moves from not-started to complete. Exact rollup:
**187 complete · 26 active · 79 not started · 0 blocked**; unfinished becomes
**105** and actionable proof/docs falls from 7 to **6**.

The implementation already contained the hard correctness mechanisms:

- `capability_rows` destructures `ModelCapabilities` exhaustively, so adding a
  field cannot silently omit it;
- static provider route rows derive from the exact inference/configured-only
  constants used by provider admission/error text;
- `docs/reference/capabilities.md` is generated and ordinary CI compares it
  byte-for-byte; per-model facts are explicitly excluded because they are live,
  credentialed catalog evidence.

The missing product edge was discoverability. Root README, engineering evidence
and STATUS now link the generated reference rather than copying its table. The
sanctioned regeneration test ran once after OpenAI/Anthropic moved into the
inference route list and passed. No live catalog/provider request is part of
DOC03 (GOTCHAS #229).

## 2026-08-30 — U16/CMD07 neutral inspector surface active

Tracker: `U16` and `CMD07` move from not-started to active. Rollup becomes
**187 complete · 28 active · 77 not started · 0 blocked**; unfinished and
scheduling buckets remain **105** / 7 external / 55 implementation / 6 proof /
31 dependency-blocked / 6 optional.

Default plugin `status-context` now contributes immediate `/context` and
`/usage` from the existing command/model services. It adds no service key or
schema migration, so exact profiles may omit it.

- `/context` renders every latest Agent envelope contributor and better-ranked
  refusal, preserving exact, provider/local-estimated and uncounted evidence;
  it joins the matching durable request window and cached model pricing, so
  headroom/cost remain bounds or Unknown rather than fabricated numbers;
- `/usage` projects JSONL turns/routes/tokens/completeness and derives cost only
  when every usage/route/input+output price exists. Turn detail is bounded to
  20 rows;
- both surfaces explicitly say detailed cache facts are unavailable because
  POA05/PAN05/PAN06 metadata is not yet neutral/durable;
- `/compact list|<strategy> [keep]` reads the live registry. `list` writes no
  model/session state and a numeric-only argument retains historical portable
  keep behavior.

Evidence: warnings-denied clippy passes across LLM/Agent/status/CLI; status is
green at **26 tests**, Agent compact and LLM context families pass, and exact
default loader/inventory/plugin-attribution targets pass after updating the one
authoritative command metadata table. Unix default composition is now **90
plugins**; service keys remain 60. Raw source inventory is **3,134**. U16/CMD07
remain active until provider cache/edit facts survive restart and render
meaningfully (GOTCHAS #230).

## 2026-08-30 — POA05 normal Responses cache projection closed; durable plane still active

The OpenAI provider's model-gated `prompt-cache` option now reaches ordinary
Responses calls. The shared adapter binds an exact option kind to an explicit
member-to-top-level-field dialect, rejects incomplete/extra objects and rejects
configuration that duplicates or collides with protocol-owned request fields.
The production provider admits its configured option, marks the resolved call
with `PromptCache`, and a transport fixture proves `prompt_cache_key` plus
`prompt_cache_options` arrive together on the `store:false` body.

Focused evidence: the new shared protocol and production-provider transport
targets pass, and warnings-denied clippy is green for `heycode-llm` plus
`heycode-provider-openai`. Accounted focused totals become **386 LLM** and **46
OpenAI** tests; raw source inventory becomes **3,136** before the separate
session lane is integrated. POA05 stays `[~]`: detailed response counters still
need a neutral durable event/value, Settings must own the cache policy/key, and
`/context`/`/usage` must render the restart-surviving facts (GOTCHAS #231).

## 2026-08-30 — K11 managed profile admission accepted

Tracker: `K11` moves from not-started to complete. Exact rollup becomes **188
complete · 28 active · 76 not started · 0 blocked**; unfinished becomes **104**.
K11 leaves actionable implementation at **55** because dependency-ready PL08
replaces it, while dependency-blocked falls from 31 to **30**. Other buckets
remain 7 external, 6 proof/docs and 6 optional.

Standalone profile schema v2 keeps schema-v1 plugin rows readable and adds one
managed-only constraints section. It can allow exact implementation sources and
deny broad descriptor capability families. Empty/duplicate rules and v1/non-
Managed attempts fail at admission. `PluginFactories::build_profile` constructs
the selected descriptors, enforces every final rule and returns them only after
the whole set passes; it invokes no plugin `apply`. The production loader now
uses this API, and a real-composition regression denies `external_process`
before a Context exists.

Four K11 config tests prove source rejection, capability rejection, authority
rejection and allowed deterministic order. The complete config gate is **64
tests**; production K11 and exact session-recomposition root targets pass; CLI
accounting is **117**. Warnings-denied clippy is green across core/config/CLI
and every transitive crate it checked. No full-workspace or platform claim is
made (GOTCHAS #232).

## 2026-08-30 — U15/CMD06 session lifecycle vertical accepted

Tracker: `U15` and `CMD06` move from not-started to complete. Exact rollup is
**190 complete · 28 active · 74 not started · 0 blocked**; unfinished becomes
**102**. Actionable implementation falls to **54** and dependency-blocked to
**29**; the other buckets remain 7 external, 6 proof/docs and 6 optional.

The effect-owned JSONL query service now owns deterministic ten-row pages with
bounded text/storage/lineage/status/source/cwd/runtime filters plus durable
create/resume/fork/rename/archive/delete/restore/export. The TUI registers a
sessions panel and queued `/new`, `/resume`, `/fork`, `/rename`, `/archive`,
`/delete`, `/export`; corrupt entries remain visible errors, delete defaults to
Cancel, and new/resume/fork return typed outcomes only after commit/readback.
CLI replaces every prior resume selector with the exact committed session path,
preserves all unrelated arguments and uses the ordinary shutdown/runtime-drop/
startup recomposition boundary.

Root review added the missing production edges and hardened the lower race:

- config schema v23 inserts `session-query-jsonl` before historical exact TUI
  Consumers;
- command metadata, exact contribution and `panel:sessions` inventories match
  the live world;
- one owner-only cross-process root lock now serializes direct/service forks
  with delete, restore and export, closing the descendant scan→rename race;
- fork/delete directory mutations checkpoint parent directories, and lossless
  export re-hashes each physical suffix against the snapshot validated at open.

Focused evidence: config **65**, session **145**, TUI **149** and CLI **117**
tests pass; warnings-denied clippy is green across core/config/session/TUI/CLI
and their checked transitive graph; format/diff checks are clean. Raw source
inventory is **3,176**. Off-Unix destructive lifecycle mutation remains
Unsupported, and C10 still owns redacted support bundles. No full-workspace,
live-provider or cross-platform claim is made (GOTCHAS #233).

## 2026-08-30 — U16/CMD07 durable detailed-response inspection accepted

Tracker: `U16` and `CMD07` move from active to complete. Rollup becomes **192
complete · 26 active · 74 not started · 0 blocked**; unfinished reaches **100**.
Actionable implementation falls from 54 to **52**; other buckets remain 7
external, 6 proof/docs, 29 dependency-blocked and 6 optional.

Core now owns schema-v1 `ProviderResponseMetadata`: exact cache read/write,
optional uncached and 5m/1h partitions, optional reasoning, applied thinking/
tool clearing counts and explicit cache-prefix impact. Impossible partitions,
duplicate edits, empty facts and unsupported schema fail with closed safe
errors. OpenAI Responses maps current detailed usage when present and requires
it for configured cache calls; Anthropic maps its validated provider-local
cache/edit report. Both emit one neutral event before Usage/Finish.

Agent buffers that event with all strict output, checks its totals against
normalized Usage and commits only after terminal Finish as v2-only
`assistant/response-metadata`. Session request projection rejects orphan,
duplicate, turn/step-mismatched and invalid facts and exposes the correlated
metadata after restart; model/provider input projection ignores it.

`/context` renders the latest detailed fact and `/usage` bounds detailed
response rows separately from turn rows. Absence and corrupt projection render
distinctly. Cache-aware cost requires a provider-proven uncached/read/write
partition and every used published price; an OpenAI subset without non-overlap
evidence remains Unknown rather than being subtracted or double-billed.
`/compact list|<strategy> [keep]` remains the live strategy Consumer.

Focused package suites passed except for two diagnosed stale fixtures (the new
closed-kind row and a legacy fake test with no request header); their exact
corrected targets passed. Warnings-denied clippy is green across core/LLM/
session/OpenAI/Anthropic/Agent/status and diff checks are clean. Accounted
focused totals become core **68**, LLM **387**, session **146** and status **27**;
Agent stays **164**, OpenAI **46**, Anthropic **55**. POA05/PAN05/PAN06 remain
active only for provider-owned opt-in Settings/aligned policy; no live call,
full-workspace or platform claim is made (GOTCHAS #234).

## 2026-08-30 — PL08 lower managed-policy gate active; full workspace milestone green

PL08 moves from not-started to active, leaving total unfinished unchanged:
**192 complete · 27 active · 73 not started · 0 blocked**. Scheduling buckets
remain 7 external / 52 implementation / 6 proof-docs / 29 dependency-blocked /
6 optional.

The extension boundary now evaluates eight independent managed axes—source,
channel, publisher namespace, exact version, catalog/package digest, signature,
platform and capability. Only all-Allowed authorizes. Missing rules deny;
signature presence and manifest upstream checksum remain Unknown when no
cryptographic verifier/fetch receipt exists. Installation freezes and hashes
the source tree before policy and commits those same bytes only after PL05 plus
PL08 admission. Activation resolves/rehashes the immutable object, reapplies
current policy, freezes declarative documents and only then permits host calls.

Root review keeps PL08 active: ordinary PL06 install/enable/update/rollback
state has no pinned marketplace generation/current managed policy and does not
yet route through the new constructors. A library gate does not prove the
bypass is gone (GOTCHAS #235).

The complete current tree then passed one full milestone gate:

- `cargo fmt --all --check` — green;
- `cargo clippy --workspace --all-targets -- -D warnings` — green;
- `cargo test --workspace --no-fail-fast` — **3,176 tests**, 0 failed, 0
  ignored, plus **6 doctests**, all green.

Raw source inventory is **3,191** Rust test attributes; platform-gated/helper
attributes account for the difference from this host's executed list. The gate
includes 105 extension tests and every new session/response-metadata/provider/
inspector path. It is not live-provider or cross-platform evidence.

### PL08 root admission closure

Root then closed the bypass identified above and moved PL08 to complete. Final
rollup: **193 complete · 26 active · 73 not started · 0 blocked**; unfinished is
**99** and actionable implementation falls to **51**.

`PluginLifecycle` now accepts an explicit `PluginLifecycleAdmission` for every
authority-increasing transition. Production `heycode plugin` supplies
`RequireManagedPluginPolicy`: absent authority denies install/enable/update/
rollback before cache/state mutation, while disable/remove remain available.
`ManagedLifecycleAdmission` is the positive path: it binds one exact cache,
marketplace source/catalog generation, host and policy and calls
`prepare_managed_declarative` before lifecycle publication. Low-level PL06
tests retain the explicitly unmanaged constructor; the product root does not.

Two regressions cover default deny/reduction and allowed cached-version recheck.
Extension accounting becomes **107** and CLI **118**; both focused tests and
warnings-denied clippy are green. This closure landed after the 3,176-test full
gate, so no newer full-workspace claim is made.

## 2026-08-30 — C10 structural redacted session export accepted

Tracker: `C10` moves from not-started to complete. Rollup becomes **194
complete · 26 active · 72 not started · 0 blocked**; unfinished reaches **98**
and actionable implementation falls to **50**.

`SessionExportFormat::RedactedSupport` adds the third C10 form beside exact
lineage JSONL and bounded Markdown. Its schema-v1 JSON contains only static
event kind names, sequence/time, closed outcome names, counts/booleans and
numeric usage/cache/edit facts. Prompt/answer/reasoning/tool/provider/URL/title/
path/attachment-name/opaque-id values have no field. The newest 10,000 logical
events fit under an 8-MiB ceiling with an explicit omitted-prefix count.

TUI `/export [session] support` uses the same effect-owned lower service. A
real session bearing a credential-shaped canary, private prompt/answer/
reasoning/title/cwd and id proves none serialize while structural event/usage
facts remain. Session **147** and TUI **149** tests plus warnings-denied clippy
are green. Q18/CMD12 still own joining this trace with config/plugin/health/
version/platform evidence, preview, runbook and transmission approval
(GOTCHAS #236). No newer full-workspace claim is made.

## 2026-08-30 — POA05/PAN05/PAN06 provider policy activation accepted

Tracker: `POA05`, `PAN05` and `PAN06` move from active to complete. Rollup
becomes **197 complete · 23 active · 72 not started · 0 blocked**; unfinished
reaches **95** and actionable implementation falls from 50 to **47**. The
other buckets remain 7 external, 6 proof/docs, 29 dependency-blocked and 6
post-beta/optional.

OpenAI's provider plugin now effect-registers restart-applied, wire-exposed
`openai-prompt-cache` Settings. The default is disabled and carries no key;
the enabled form requires a bounded key that passes the shared credential
screen plus an explicit implicit/explicit mode. Root resolves the exact
snapshot after namespace registration and applies it before publishing the
strict provider, so ordinary Responses calls receive both cache members or
neither.

Anthropic's provider plugin owns one restart-applied `anthropic` namespace.
It defaults prompt caching and context editing off, distinguishes automatic
5m/1h caching, and represents thinking/tool clearing with explicit modes and
positive values rather than hidden zero sentinels. One
`AnthropicSettingsPolicies` generation configures both the inference provider
and `/count_tokens`, preventing pressure estimates from silently using a
different edit policy. Config schema v24 repairs historical exact provider and
counter profiles by inserting Settings and the full provider-owner chain.

Production-composition tests use owner-only isolated settings and credential
roots, inspect the enabled request policies and issue no network request. The
broad focused provider/config/CLI suites are green at OpenAI **50**, Anthropic
**63**, config **66** and CLI **118** tests; warnings-denied clippy is green for
all four packages and the exact live-inventory regression passes. This closure
landed after the 3,176-test full gate, so no newer workspace-wide, live-provider
or cross-platform claim is made (GOTCHAS #237).

## 2026-08-30 — PL07 deterministic graph and P10 provider waterfalls accepted

Tracker: `PL07` moves from not-started to complete and `P10` moves through
active to complete only after its exact current-consumer clause is satisfied.
Rollup becomes **199 complete · 23 active · 70 not started · 0 blocked**;
unfinished reaches **93**. Actionable implementation is **46** because P10
unlocks N05 while PL07 leaves no new dependency edge; dependency-blocked falls
to **28**. Other buckets remain 7 external, 6 proof/docs and 6 optional.

PL07 consumes a complete validated manifest generation plus one explicit host
platform. Required and present-optional dependencies use SemVer precedence and
inclusive-minimum/exclusive-maximum bounds; selected conflicts are symmetric;
duplicate ids, missing/incompatible dependencies, cycles and unsupported
platforms retain distinct closed diagnostics. Stable Kahn ordering is
dependency-first with plugin-id ties. The opaque graph gates frozen PL02 cache
publication, one-write PL06 lifecycle reconciliation and ordinary/managed PL03
activation before mutation. Large diagnostics and wrapper variants are boxed;
public error-size regressions keep ordinary Result paths bounded. Candidate
discovery/version acquisition and concrete PL03 hosts remain separate rows.

P10 adds `provider-interception` to every `llm` implementation with exact
`provider/request` and `provider/response` inventory. Core's checked waterfall
reports whether `next` reached the terminal delegate. Request layers can mutate
only fields independently persisted in C02; provider/model/catalog/input
chronology is read-only, the adapter revalidates, and C05 still compares before
transport. Every strict adapter publishes a secret-free authentication preview
which final resolution must match. Agent's native-tool Consumer validates the
post-downstream route subset and rechecks immediately before resolve.

Response layers receive either one normalized event or only the body-free
`ProviderErrorClass`; raw `LlmError` text/body/URL never enters middleware.
Optional default `provider-telemetry` records the existing closed
`RequestFailed` dimensions through local-off or opt-in OTLP. Refusal cancels
the adapter operation, parked layers settle under caller cancellation, layer
error bodies are discarded and an untyped missing-`next` path fails closed.
Native child Agents share the service; compatibility Chat and distinct native
compaction remain explicitly outside it.

The OTLP replacement exposed an ordering defect: a higher profile layer could
append a valid provider after its base Consumer. Concrete scoped factory build
now performs a stable topological order over exact `provides`/`injects` after
scope/managed resolution. Providers precede Consumers, independent rows keep
their original order and scope attribution travels with the row; missing
providers and dependency cycles still fail loud (GOTCHAS #238/#239).

Focused evidence:

- extensions format, warnings-denied clippy and **119 tests** (6 unit + 113
  integration), zero failures/ignored;
- core **69**, LLM **394** and Agent **172** tests plus warnings-denied clippy,
  zero failures/ignored;
- config **67** and CLI **118** tests plus warnings-denied clippy, including
  default and opt-in OTLP real composition, exact service/plugin/inventory
  attribution and the shared production harness;
- `cargo fmt --all --check` and repository `git diff --check` green.

The current source inventory is **3,221** Rust test attributes. This work landed
after the 3,176-test full workspace milestone; no newer full-workspace,
live-provider or cross-platform claim is made.

## 2026-08-30 — N05 request-transform registry accepted

Tracker: `N05` moves from not-started to complete after P10 unlock. Rollup
becomes **200 complete · 23 active · 69 not started · 0 blocked**; unfinished
reaches **92** and actionable implementation falls to **45**. The other buckets
remain 7 external, 6 proof/docs, 28 dependency-blocked and 6 optional.

Official OpenRouter sources were refreshed before implementation. Current
documentation still defines request plugins as once-per-request mutation rather
than server tools; context compression truncates middle messages and may reroute,
file parsing exposes native/cloudflare/mistral engines with distinct billing,
and response healing is non-streaming structured-output only. Account or
workspace “Prevent overrides” can defeat request-level configuration, so heycode
continues to report effective execution as Unknown.

Core now has an exact `request_transform` inventory namespace. Optional default
plugin `request-transforms` publishes `RequestTransformRegistry` and attaches a
post-`next` request layer to P10. Provider generations validate/freeze complete
descriptor rows at effect registration, sort by exact id and remove by token on
rollback/shutdown. Disabled rows carry no cost. Enabled rows require one
explicit cost evidence value: Unknown, documented free, ordinary upstream input
tokens or a validated non-zero per-thousand-page price.

Provider plugin `request-transforms-openrouter` maps the existing POR06 policy
into three rows without moving provider knowledge into the shared crate. The
P10 layer inserts an absent exact provider option, accepts byte-semantic equality
and rejects same-provider/kind conflict without overwrite. The current
production all-disabled option is therefore independently visible and verified;
adapter validation and C05 remain the transport gate. The deprecated OpenRouter
web plugin remains outside N05 because POR05 owns its server-tool replacement.

Focused evidence: formatting and warnings-denied clippy are green for core,
LLM, OpenRouter and CLI; core **69**, LLM **398**, OpenRouter **38** and CLI
**118** tests pass with zero failures/ignored. This landed after the 3,176-test
full workspace milestone; no newer full-workspace, live-provider or
cross-platform claim is made (GOTCHAS #240).

## 2026-08-30 — N06 durable native/local tool usage accepted

Tracker: `N06` moves from not-started to complete. Rollup becomes **201
complete · 23 active · 68 not started · 0 blocked**; unfinished reaches **91**.
Actionable implementation stays **45** because TEL04 unlocks as N06 leaves;
dependency-blocked falls to **27**. Other buckets remain 7 external, 6
proof/docs and 6 optional.

Official OpenRouter server-tool documentation confirms that
`web_search_requests` is an aggregate number of model-generated search queries,
while engine pricing varies: Exa/Parallel/Perplexity may use fixed request fees,
Firecrawl consumes separate credits and native search passes provider pricing
through. Current heycode policy selects `auto`, so exact request count does not
prove an exact cost.

Core now validates `ServerToolUsage`: positive aggregate requests, explicit
`ProviderAggregate` evidence and either Unknown or a published non-zero
pico-unit total. Chat emits the real OpenRouter aggregate before Usage/Finish
and still emits no synthetic `ServerToolCall`. Agent validates and buffers it
with the strict response group, committing v2-only `server-tool/usage` only
after terminal Finish. Request projection requires a matching request/turn/
step and one aggregate per request/logical id; v1 cannot claim the kind.

TEL01 usage projection now keeps three separate planes: local exact calls,
provider exact calls/results and provider aggregate requests. Exact outcomes
and unsettled counts derive only from correlated ids. Duplicate invalid rows do
not inflate the infallible view, while request projection still rejects them.
`/usage` renders logical/source/request/success/error/unsettled/cost facts, caps
none of those into zero and has no field for query, arguments, result content or
provider bodies. C10 support export retains structural counts/cost only.

Focused evidence: formatting and warnings-denied clippy are green across core,
LLM, session, Agent, status, OpenRouter and downstream CLI; core **70**, session
**148**, status **28**, LLM **398**, Agent **172** and OpenRouter **38** tests
pass with zero failures/ignored. No newer full-workspace, live-provider or
cross-platform claim is made (GOTCHAS #241).

## 2026-08-30 — TEL04 committed product metrics accepted

Tracker: `TEL04` moves from not-started to complete. Rollup becomes **202
complete · 23 active · 67 not started · 0 blocked**; unfinished reaches **90**
and actionable implementation falls to **44**. External evidence/approval stays
7, proof/docs 6, dependency-blocked 27 and optional 6.

Telemetry event schema v2 adds one positive aggregate `count`; v1 remains
readable as one observation and zero fails. Local counters add that count and
OTLP represents it as a delta sum. The closed name/dimension vocabularies now
cover provider request, tool execution plane, compaction and cache activity
plus purpose/lineage/runtime. No prompt, tool argument/result, path, provider
body or arbitrary failure text has an event field.

Optional default `telemetry-metrics` injects the authoritative session and the
selected telemetry Provider. At apply it reads the prefix only to seed bounded
lineage/runtime/request-route correlation; historical events emit no metrics.
It then subscribes to the session-owned bus, whose events publish after durable
append. New request headers, local calls, exact provider calls, provider
aggregate usage, portable/native compaction and cache observations become
closed events. Four exact `telemetry_metric` inventory rows and the listener are
Context effects. Local-off remains structurally unable to emit and an opt-in
OTLP profile receives the identical Consumer contract.

Focused evidence: telemetry **119**, telemetry-OTLP **15** and the Agent
committed-metrics integration pass; the subsequent combined Agent gate passes
all **173** tests. Formatting, warnings-denied clippy and downstream CLI
composition are green. This is deterministic local evidence only; no live
collector or newer full-workspace claim is made (GOTCHAS #242).

## 2026-08-30 — Q03 journey harness and U19 accessible shell accepted

Tracker: `Q03` and `U19` move from not-started to complete. Rollup becomes
**204 complete · 23 active · 65 not started · 0 blocked**; unfinished reaches
**88** and actionable implementation falls to **42**. The other scheduling
buckets remain 7 external, 6 proof/docs, 27 dependency-blocked and 6 optional.

The TUI now has one bounded screen-reader projection over production
`AppState`, not a parallel application. It preserves modal priority, focus and
the ordinary key router; strips terminal-control payloads; suppresses unchanged
frames; and emits no alternate-screen, cursor-control, color or animation
bytes. Automatic `TERM=dumb` and explicit `TuiDisplayMode::ScreenReader` select
it. The shipping CLI exposes `--screen-reader` only for interactive startup,
routes it into that exact mode and preserves it across connection/profile/
session/trust recomposition. Other modes reject it explicitly.

Q03's reusable recorder applies typed host actions, terminal events and UI
events to the same reducer. Fixed fixtures pin exact trust, first-run setup,
command-palette, MCP-management and provider/runtime frames without a clock,
network, subprocess or terminal timing dependency.

Focused evidence:

- TUI formatting, all-target warnings-denied clippy and **158 tests** pass;
- CLI all-target warnings-denied clippy and **119 tests** pass;
- the combined Agent/CLI gate passes **292 tests**, zero failures/ignored;
- an isolated temporary-home real-binary `--restricted-workspace --fake
  --screen-reader` PTY smoke prints the flat Welcome/Status/Composer frames,
  reaches healthy `3 passed; 0 warnings` and exits cleanly on double Ctrl+C;
- `cargo fmt --all --check` and `git diff --check` pass.

Current source inventory is **3,244 Rust test attributes across 395 files**.
This landed after the 3,176-test full workspace milestone; no newer full-
workspace, provider-network or cross-platform claim is made (GOTCHAS #243).

## 2026-08-30 — P09 production operation-time credentials accepted

Tracker: recovered candidate `P09` moves from active to complete. Rollup becomes
**205 complete · 22 active · 65 not started · 0 blocked**; unfinished reaches
**87** and actionable implementation falls to **41**. External evidence stays
7, proof/docs 6, dependency-blocked 27 and optional 6. Four recovered candidates
remain pending owner acceptance.

The audit separated protocol capability from product reachability. Shared Chat,
Responses, Anthropic Messages, Gemini and Bedrock adapters already acquired a
`RouteCredential` once per operation, reused it across retries and resolved
again for the next operation. Production `credential_llm_plugin` nevertheless
resolved the selected reference during apply and passed a String into fixed-key
constructors for DeepSeek, OpenRouter, OpenAI and Anthropic. The first new
production assertion observed `AdapterOwned`, proving rotation still required
world recomposition despite the lower green matrix.

Root now keeps startup presence/validation preflight but drops that value after
the validation fingerprint is seeded. It constructs one registry-backed exact
route and passes it into all four production providers. OpenAI and Anthropic
gained route-credential constructors; their normal adapters and native
compaction operation share the same binding. DeepSeek/OpenRouter reuse their
existing credential constructors. A resolver-route mismatch returns before
registry access, a missing route never probes another reference, and strict
request auth evidence records the configured custom handle. Explicit fixed-key
embedding/test constructors remain `AdapterOwned`.

Focused evidence:

- formatting and all-target warnings-denied clippy pass across credentials,
  LLM, OpenAI, Anthropic and CLI;
- credentials **11**, LLM **398**, OpenAI **50**, Anthropic **63** and CLI
  **119** tests pass: **641 total**, zero failures/ignored;
- shared fixtures rotate between operations, reuse one value across retry and
  refuse cross-route access before transport;
- real production compositions bind custom DeepSeek, OpenRouter, OpenAI and
  Anthropic references without network traffic;
- `git diff --check` passes.

The source inventory remains **3,244** test attributes because the production
assertions strengthen existing composition journeys. No newer full-workspace,
live-provider or cross-platform claim is made (GOTCHAS #244).

## 2026-08-30 — A05/U18 native composer delivery accepted

Tracker: `A05` and `U18` move from not-started to complete. Rollup becomes
**207 complete · 22 active · 63 not started · 0 blocked**; unfinished reaches
**85** and actionable implementation falls to **39**. External evidence stays
7, proof/docs 6, dependency-blocked 27 and optional 6.

Native active composer input now has three non-overlapping meanings. Enter
creates a durable Steer for the next model step; persisted/rebindable keymap
action `queue-follow-up` uses Tab by default and creates a next-turn FollowUp;
Esc only invokes the active cancellation owner. Idle Enter remains fresh send,
idle Tab remains textarea input and slash commands retain U11's Enter-driven
scheduling plane.

The TUI never echoes queued text. It calls `Agent::submit_inbox`, renders only a
safe delivery label and exact next-turn/next-step counts, and receives user text
only after Agent's atomic claim commits the removal plus `user/message` and
publishes `UserEcho`. Full and flat renderers expose the state-specific keys.
One Wake is stored until turn/command settlement, consumed once and starts one
caller-token-owned/joined follow-up before queued commands; later messages need
later settlement wakes. Startup seeds pending state from the durable session.

One lifecycle race required an explicit conversion. The AppState can still say
active after Agent settlement. If a Steer append therefore returns idle Wake,
the next-step occurrence is durably canceled and the same text is appended as
FollowUp rather than being stranded. If the effective runtime is delegated,
Enter/Tab preserve the composer and report unavailable; they never target the
different native Agent while looking successful.

Focused evidence:

- UI all-target warnings-denied clippy and **51 tests** pass;
- TUI all-target warnings-denied clippy and **164 tests** pass;
- all **8** Agent inbox integration tests pass, including busy re-drain,
  atomic claim, one-message follow-up and wake behavior;
- the former busy status test was repurposed to require native Enter/Tab/Esc
  meaning rather than preserving the obsolete reasoning/palette footer;
- formatting and `git diff --check` pass.

Current source inventory is **3,250 Rust test attributes across 396 files**.
This is a focused local gate after the 3,176-test workspace milestone; no newer
full-workspace, delegated-control, provider-network or cross-platform claim is
made (GOTCHAS #245).

## 2026-08-30 — O02/O03 native subagent modes and authority accepted

Tracker: `O02` and `O03` move from not-started to complete. Rollup becomes
**209 complete · 22 active · 61 not started · 0 blocked**; unfinished reaches
**83** and actionable implementation falls to **37**. O02 unlocks P3 O07, so
dependency-blocked falls to **26** and optional/actionable rises to **7**;
external evidence remains 7 and proof/docs 6.

O02 acceptance was already substantially implemented but had never been
audited as a product claim. Fresh creates a metadata-bound durable child and a
recorded request proves it is blind to parent history. ForkParent uses C07's
verified shared-prefix lineage, including the first-open-turn boundary, and
stores no copied prompt/history marker. Continuable retains one child Agent and
session across `send_message`; OneShot leaves no live handle. Native provider
evidence for fork/continuation/interrupt is exactly Supported and unproven
providers cannot win selection.

O03 found a real cross-child authority defect. The effect-owned registry kept
all continuable handles in one global map and its model-facing tools listed/
looked up by id without an owner. A nested child could therefore see or control
a sibling. `SubagentAuthority` now carries a private registry token, opaque
owner id, depth and retention right. The registry alone mints a root; child
authority derives token + child session id + depth+1. Every retained handle is
owner-bound, and list/send/interrupt/close return no evidence for a foreign id.
Unbound requests fail before provider execution.

Authority lifetime is also enforced. A one-shot child cannot create a
continuable descendant; a continuable handle scopes its stored authority on
every later send, so depth cannot reset. Provider output is admitted before
publication: one-shot/continuable handle presence, handle identity and live-id
uniqueness must match, and rejected handles are closed. Context remains the
terminal owner and interrupts all live children regardless of owner.

Focused evidence:

- Agent/CLI all-target warnings-denied clippy and formatting pass;
- Agent **178** and CLI **119** tests pass: **297 total**, zero
  failures/ignored;
- the focused subagent family has **20** green tests covering blind/forked/
  continuable sessions, first-turn lineage, capability selection, owner
  isolation, one-shot retention, follow-up depth, provider-output admission and
  Context disposal;
- `git diff --check` passes.

Current source inventory is **3,255 Rust test attributes across 396 files**.
This is a focused local gate after the 3,176-test workspace milestone; no newer
full-workspace, worktree/team UI, provider-network or cross-platform claim is
made (GOTCHAS #246).

## 2026-08-30 — QSEC02 unified secret canary accepted

Tracker: `QSEC02` moves from not-started to complete. Rollup becomes **210
complete · 22 active · 60 not started · 0 blocked**; unfinished reaches **82**
and actionable verification/evidence/docs falls from 6 to **5**. Other buckets
remain 7 external, 37 implementation, 26 dependency-blocked and 7 optional.

The first red test exposed a real credential error leak. `CredentialProvider`
returns a free-form String under a redaction contract, but the registry copied
that body into `CredentialsError::Provider` and `CredentialResolutionError`.
A test Provider returning the canary made public Display and Debug print it.
The registry now discards provider bodies at every inspect/resolve/write/delete
boundary and retains only the validated provider id; route resolution adds only
the requested non-secret reference. A future actionable detail requires a
closed code rather than reopening free text.

The cross-crate product journey uses one map-backed environment canary and a
strict DeepSeek adapter over a recording mock transport. Positive premises
prove the environment provider resolves the exact value, the Authorization
header receives it and a purpose-built process spec physically contains it.
The same value is absent from credential/route/process Debug, credential
descriptor JSON, provider request body and system prompt, UI events, projected
request, physical session JSONL and the committed `RedactedSupport` artifact.
Only the outbound auth boundary observes it.

Focused evidence:

- formatting and all-target warnings-denied clippy pass across credentials,
  credentials-env, Exec, Session, LLM, Agent and CLI;
- the combined gate accounts credentials **12**, credentials-env **2**, Exec
  **81**, Session **148**, LLM **398**, Agent **178** and CLI **120** tests;
- the changed-package tail is independently green at credentials **12** and
  CLI **120**, zero failures/ignored;
- `git diff --check` passes.

Current source inventory is **3,257 Rust test attributes across 397 files**.
This is a deterministic local gate after the 3,176-test workspace milestone;
no live credential/provider, newer full-workspace or cross-platform claim is
made (GOTCHAS #247).

## 2026-08-30 — Q04 persisted request replay oracle accepted

Tracker: `Q04` moves from not-started to complete. Rollup becomes **211
complete · 22 active · 59 not started · 0 blocked**; unfinished reaches **81**
and actionable verification/evidence/docs falls to **4**. Other buckets remain
7 external, 37 implementation, 26 dependency-blocked and 7 optional.

`heycode_agent::testing::verify_persisted_replay` is now the reusable Q04
boundary. A fixture seeds its model-visible Session inputs and supplies one
live `ResolvedCall`; the oracle commits the exact C02 header/context, flushes,
reopens the physical JSONL, selects the exact request id with
`project_requests` and invokes production C05 against the exact adapter.
Success returns the ownership-consuming `VerifiedResolvedCall`, not a weaker
parallel result.

One matrix applies that operation to OpenAI Chat Completions, OpenAI Responses,
Anthropic Messages, Gemini GenerateContent and Bedrock Converse without any
transport call. A corruption control proves persisted independence. The first
attempt appended `not-json` through another descriptor, but the live Session's
older write cursor overwrote that tail when it appended snapshots, so the
oracle correctly succeeded. The effective mutation overwrites an existing
envelope byte: in-memory events remain valid while reopen fails with one safe
body-free class that does not echo the system canary.

Focused evidence:

- Agent formatting and all-target warnings-denied clippy pass;
- Agent **180 tests** pass, zero failures/ignored;
- both oracle tests pass across all five protocols and the physical corruption
  control;
- `git diff --check` passes.

Current source inventory is **3,259 Rust test attributes across 398 files**.
This is a deterministic local gate after the 3,176-test workspace milestone;
no provider-network, newer full-workspace or cross-platform claim is made
(GOTCHAS #248).

## 2026-08-30 — CMD04 production panel path reactivated

Tracker: `CMD04` moves from not-started to active. Exact rollup becomes **211
complete · 23 active · 58 not started · 0 blocked**; unfinished remains **81**
and the actionability buckets do not change.

The earlier partial path proved only a TUI-local `/mcp` inbox. The shipping
loop now accepts the same Settings-backed `McpManagement` and managed
`PluginLifecycle` constructors as standalone CLI management, plus an exact
verified plugin-cache projection. It also receives the composed `SkillSet`,
`SubagentRegistry` and `HookService`. Default composition adds
`plugin-lifecycle` and mounts `panel-commands` after the TUI handle exists.

`/mcp`, `/agents` and `/hooks` are immediate bridge commands with live
availability. Existing capability owners keep their ids: `/plugins` with no
argument emits a validated panel request while `verbose` retains the attributed
inventory, and `/skills` emits the same human-only request while `/skill`
remains model-scheduling. Skills, agents and hooks share one bounded read-only
panel that never renders skill bodies, authority-scoped child ids, hook
commands/prompts/arguments or terminal controls. The visual and screen-reader
renderers project that exact view.

Focused compile is green across Agent, skills, TUI and CLI. The first combined
test command did not reach this batch: the concurrently active C09 lane had
published `heycode-session::index` between compile and test, and that mid-edit
module lacked the Unix `atomic_write_file::OpenOptionsExt` import required by
`preserve_mode`. The owning lane received the exact diagnosis. Per the
no-unchanged-loop rule, CMD04 remains active and its test/clippy evidence is not
claimed until that shared dependency settles (GOTCHAS #249).

## 2026-08-30 — C09 and Q17 accepted

Tracker: `C09` and `Q17` move from not-started to complete. Exact rollup becomes
**213 complete · 23 active · 56 not started · 0 blocked**; unfinished reaches
**79**. Actionable implementation falls to **36** and actionable independent
proof/docs to **3**; the other buckets remain 7 external, 26
dependency-blocked and 7 optional.

`SqliteSessionIndex` is a bounded disposable database beside the sessions
root. Compare and rebuild first use the ordinary query/lineage reader across
active, archived, root and shared-prefix fork JSONL, then bind each safe summary
to the exact logical-source hash. Missing, stale, corrupt, interrupted and
incompatible SQLite states are rebuild reasons; invalid JSONL is a hard truth
failure. Rebuild holds the cross-process lineage lock, writes a private staging
database, re-projects JSONL before publication and atomically replaces the
owner-only index only when both generations match. A last-good index remains
byte-identical across invalid truth.

Q17 adds config fixtures for unversioned and every schema v1–v24 plus a newer
v25 refusal. Every historical plan creates a byte-exact backup, upgrades once,
replans to none and re-applies idempotently; downgrade guidance distinguishes a
compatible reader, restoreable backup and required compatible copy. Session
fixtures cover v1, v2, v1-prefix/v2-suffix and future v3: v1 bytes remain
unchanged, the next append is v2, version regression/v2-only kinds in v1 fail
loud, and newer logs remain untouched. The generated-profile case consumes the
live CMD04 rows rather than adding a migration special case.

Focused evidence from the owning task is green: formatting, all-target
warnings-denied clippy and **225 tests**—config **70** plus session **155**,
zero failures/ignored. Root reviewed the source/acceptance and hoisted
`rusqlite 0.30` with bundled SQLite into workspace dependencies; the resolved
lock entries were already present. No cross-platform native SQLite or newer
full-workspace claim is made (GOTCHAS #250).

## 2026-08-30 — X01 and R11 accepted; R10/X07 remain active

Tracker: `X01` and `R11` move from not-started to complete; `R10` and `X07`
move from not-started to active because implementation evidence exists but
their product/live acceptances do not. Exact rollup becomes **215 complete · 25
active · 52 not started · 0 blocked**; unfinished reaches **77**. Actionable
implementation falls to **35**, dependency-blocked to **25**, and the other
buckets remain 7 external, 3 independent proof/docs and 7 optional.

X01 now drives the real ACP server loop over a scripted native Agent tool call.
Ask-mode sends one `session/request_permission` with two offered options; only
the exact pending JSON-RPC id plus `selected/allow_once` resumes the tool.
Wrong, duplicate, late, deny and cancelled responses execute it zero times.
Cancellation denies the pending approval, the original prompt settles, and a
later prompt proves the session remains healthy. The approval task/token remains
session-owned and is joined before Context shutdown.

R11 adds a generic ACP v1 `AgentRuntime` boundary with strict 4-MiB fragmented
NDJSON framing, bounded JSON shape, exact absolute program/cwd/argv/explicit
environment specs and an abstract caller-owned process factory. A probe/session
handshake discovers session-scoped provider/model options, publishes an
Unknown-safe catalog, applies exact model selection and normalizes thought,
tool, permission and final events through the shared runtime normalizer. Cancel
settles pending permission and process ownership once; close is idempotent.

`opencode_acp_runtime` supplies the current profile contract but no heycode-exec
process plugin or installed Ox smoke, so R10 remains active. The SDK's
VS-Code-shaped transport fixture proves one host/session/correlated permission
loop, not a real installed extension, so X07 remains active.

Focused evidence: formatting and warnings-denied clippy pass for Runtime, SDK
and CLI; Runtime **20**, SDK **5**, CLI units **35** pass. Root reconciled the
three concurrent central inventory baselines and the CLI integration suite is
**89/89** green. No live OpenCode, IDE or newer full-workspace claim is made
(GOTCHAS #251).

## 2026-08-30 — Q14/Q15/Q16 lower release batch active

Tracker: `Q14`, `Q15` and `Q16` move from not-started to active. Exact rollup
becomes **215 complete · 28 active · 49 not started · 0 blocked**; unfinished
remains **77** and the scheduling buckets are unchanged.

The install library now parses authenticated schema-v2 release manifests and
requires a host-supplied `ReleaseSignatureVerifier` to check exact manifest and
artifact bytes, Sigstore bundle, configured GitHub repository/workflow and OIDC
issuer. Only that path mints an owned non-clone `VerifiedReleaseArtifact`.
Checksum/bundle substitution, signature invalid/unavailable/unsupported and
oversize inputs are closed body-free failures.

Fresh install and updates retain exact artifact, manifest and bundle data;
updates require a channel/API approval bound to the observed current version.
A private transition journal makes interrupted publication recoverable and
rollback re-verifies the retained target. Config migration remains directional:
an incompatible target refuses with restore guidance. Stable, preview and
pinned SemVer policies plus a deterministic set of enabled-plugin API ranges
gate update and rollback.

The release workflow defines native macOS/Linux/Windows builds, GitHub
OIDC-backed artifact attestations, a signed schema-v2 manifest and fresh-home
fake-provider smokes without a long-lived signing secret. `FreshMachineMatrix`
distinguishes definitions, cross-compiles, local-native and non-zero
hosted-native observations; a fake turn can complete only the deterministic
harness, never Q16's real-provider acceptance.

Focused evidence is green: install formatting, all-target warnings-denied
clippy and **55 tests**, zero failures/ignored. All three rows remain active:
there is no concrete production verifier/updater/installer Consumer, no binding
from the live PL01 enabled-plugin generation, no executed workflow and no
native real-provider turn on all three platforms (GOTCHAS #252).

## 2026-08-30 — Q05, MCP05 and QSEC04 accepted; external activation remains active

Tracker: `Q05`, `MCP05` and `QSEC04` move from not-started to complete.
`MCP15`, `PL03` and `PL09` move from not-started to active; `PL10` remains not
started. Exact rollup becomes **218 complete · 31 active · 43 not started · 0
blocked**; unfinished reaches **74**. Actionable implementation falls to **33**
and independent proof/docs to **2**; other buckets remain 7 external, 25
dependency-blocked and 7 optional.

Q05 provides one reusable bounded success/cancel/hostile assertion contract
driven by real stdio, Streamable HTTP and OAuth fixtures. Success must make one
atomic publication, cancellation none, request counts stay bounded and hostile
diagnostics contain no control/body/canary data. This is deterministic protocol
evidence; the official inspector/real OAuth server required by MCP15 was not
run.

MCP05 adds protected-resource/authorization-server discovery plus priority-
ordered pre-registration, CIMD and DCR. Registrations bind issuer/resource/
redirect, enforce native public S256 clients, reject secret injection and
malformed/rejected registration bodies without echo. Callback/exchange security
binds redirect, state, issuer, client, resource and verifier; tokens and client
secrets reach only outbound auth and stale credential records do not cross
issuer/resource/client identities.

QSEC04 joins those OAuth attacks to bounded advisory annotations and the
extensions supply-chain suite. Read-only/idempotent hints cannot weaken exact
MCP allowlist/deny/approval. Package/executable identity, digest, capability and
contribution substitution fails before activation; crash/cancel withdraws the
complete code-plugin generation. The six declarative host methods now return
exact one-shot registrations that Context disposes even on partial failure.

PL03 and PL09 stay active: tests use abstract host/process adapters, not the
concrete product registries or heycode-exec launcher. PL10 has no WASI/WIT host.
MCP and plugin lifecycle operations are now effect-owned Context services and
held handles become terminal on rollback/shutdown, closing CMD04's hidden
composition-root construction gap.

Root supplied the final local gate after the external tasks waited on approval:
formatting and warnings-denied clippy pass; MCP **318** and extensions **128**
tests pass, zero failures/ignored. No official inspector, real OAuth server,
product code-plugin process or WASI evidence is claimed (GOTCHAS #253/#254).

## 2026-08-30 — CAT07, CAT08 and P13 accepted

Tracker: `CAT07` and `P13` move from active to complete; `CAT08` moves from
not-started to complete. Exact rollup becomes **221 complete · 29 active · 42
not started · 0 blocked**; unfinished reaches **71** and actionable
implementation falls to **30**.

CAT07 now exposes origin-retaining `CapabilityEnforcement` and
`LimitEnforcement`. Provider evidence and user assertions keep distinct source,
revision and capture facts. Assertions are computed as redundant, narrowing,
unevidenced support or contradiction; the last two fall back to provider
evidence for machine decisions, while redundant/narrowing constraints retain
their exact user source. Render never uses the machine-only value and therefore
cannot make a user claim look provider-native.

CAT08 adds strict schema-v1 catalog/protocol fixture metadata: safe provider,
official-example/redacted-capture/synthetic source kind, credential-free HTTPS
source, bounded version and non-zero capture/reconciliation time. Unknown
fields/schemas/unsafe labels/oversize documents fail body-free. Current
OpenRouter list/detail fixtures name exact official sources; DeepSeek Anthropic
parity and constructed protocol matrices remain explicitly synthetic.

P13 supplies an optional `WebSocketConnector` seam, exact bounded ws/wss
request/open frames, handshake-only reconnect budget/backoff, cancellation and
independent outcome/aggregate metrics. HTTP/SSE fallback is explicit before
connection and legal only for server-to-client requirements; bidirectional
requests fail rather than lose capability. No replay/fallback occurs after a
connection or opening-frame send. No current heycode inference protocol advertises
a WebSocket alternative, so no connector/provider endpoint is claimed.

Root supplied the task's local-socket gate: HTTP **43**, LLM **400**,
catalog-file **26**, DeepSeek **70** and OpenRouter **39** tests pass plus
doctests, zero failures/ignored; the prior warnings-denied clippy gate was
green. No authenticated provider/live-only row is promoted (GOTCHAS #255/#256).

## 2026-08-30 — CMD04 accepted through production composition

Tracker: `CMD04` moves from active to complete. Exact rollup becomes **222
complete · 28 active · 42 not started · 0 blocked**; unfinished reaches **70**
and actionable implementation falls to **29**.

Plugins `mcp-management` and `plugin-lifecycle` now publish their Settings-
backed operation owners as effect-owned Context services; held handles become
terminal on rollback/shutdown. Default composition mounts those services and
`panel-commands` after the TUI handle, and the shipping loop receives them plus
the exact `McpRegistry`, verified package index, `SkillSet`, `SubagentRegistry`
and `HookService`. The binary constructs no shadow management/lifecycle owner.

`/mcp`, `/agents` and `/hooks` use the shell's one-slot bridge and report
dynamic availability. Existing owners keep `/plugins` and `/skills`; their
no-argument path emits a validated `UiPanelId`, while `/plugins verbose` retains
exact inventory and `/skill` retains model scheduling. Skills/agents/hooks
share one bounded control-free read-only view that omits bodies, cross-owner
child ids and hook payloads. Full and screen-reader projections consume the
same state and trust/secret/onboarding/approval/question surfaces preempt it.

A real production-loader test resolves every service, executes all five
commands and opens each exact panel. Focused formatting and warnings-denied
clippy pass across Agent/skills/TUI/CLI. Accounted tests are Agent **181**,
skills **13**, TUI **168** and CLI **125**, zero product failures/ignored. One
new flat test initially expected `[Skills]` despite the established
`== Skills ==` section grammar; the test premise was corrected and its isolated
rerun is green (GOTCHAS #249/#254).

## 2026-08-30 — DOC04/DOC05 accepted; DOC02 active

Tracker: `DOC04` and `DOC05` move from not-started to complete; `DOC02` moves
from not-started to active. Exact rollup becomes **224 complete · 29 active ·
39 not started · 0 blocked**; unfinished reaches **68** and actionable
implementation falls to **27**.

Four checked-in references are generated from authoritative code projections:
the real-composition command catalog plus descriptors, root config/defaults/
schema and Settings inventory, built-in plugin order plus descriptor/manifest
vocabulary, and closed v1/v2 session kinds plus enum documentation. Root
integrated the guide/reference index and regenerated after the final
`/plugins [verbose]`, `/skills` and `mcp-servers` ownership changes.

DOC05 supplies architecture, plugin-lifecycle and session-v2 guides plus three
standalone accessible HTML/SVG diagrams. They describe Context/effect
transactions, generation swaps, commit-before-publish, JSONL migration/request
projection/repair/compaction/lineage and the exact concrete-vs-lower-boundary
gaps. Volatile provider/model facts remain linked to generated/live sources.

The one combined documentation gate is green after root reconciliation: **4
generated references fresh, 55 relative links valid across 13 Markdown files,
16 shell examples syntax-valid, 1 isolated fresh-home fake example executed and
3 diagrams accessible with the installed Diagram Design self-check**. A
generated Python bytecode cache was moved to Trash and is not part of the
artifact set.

DOC02 remains active. The setup/provider/MCP/plugin guides are present,
secret-safe and locally verified, but the executed example uses a source
checkout, temporary product home, restricted workspace and fake provider. It
does not meet a certified fresh-machine/installer/real-provider acceptance
(GOTCHAS #257).

## 2026-08-30 — PL03/PL04 concrete product activation accepted

Tracker: `PL03` moves from active to complete and `PL04` moves from not-started
to complete. Exact rollup becomes **226 complete · 28 active · 38 not started ·
0 blocked**; unfinished reaches **66** and actionable implementation reaches
**25**.

New crate `heycode-extension-host` is the dependency-correct product layer above
the host-neutral manifest/cache bridge. Strict bounded schemas activate skills,
model-scheduling slash commands, subagent presets, typed hooks, complete themes
and per-operation-credential OpenAI-compatible provider routes. The affected
registries now expose exact token-owned late registrations; provider identity
is owned `String` data rather than an artificial `'static` boundary. Dynamic
skill snapshots feed the same prompt/tool/command/TUI consumers, and preset
instructions enter the exact durable child prompt.

`DeclarativePackage` now freezes MCP documents separately and retains the
verified immutable cache root. Bundled stdio definitions canonicalize relative
command/cwd paths inside that root, require executable files plus
`mcp_connect`/`process_spawn` (and `credential_use` for referenced environment
values), and retain exact timeout/reconnect/tool-approval/exposure policy. They
are dispatched through the ordinary MCP connection plugin, so Ready publishes
only after atomic tool registration and shutdown kills the transport before
withdrawing tools.

The Unix default profile now includes `product-extensions` after `subagent`.
It lazily opens the cache during apply, resolves only enabled exact lifecycle
versions, rehashes them through PL02, dynamically publishes exact inventory and
activates inside one composition transaction. Non-Unix keeps the existing
honest unsupported owner-security boundary rather than mounting a false cache.

Evidence:

- `cargo test -p heycode-extension-host` — one real seven-kind composition passes;
  all six PL03 registries plus a live relative bundled-MCP echo tool publish and
  are empty again after Context shutdown.
- production-loader isolated-cache test — an enabled installed package reaches
  the live `SkillSet`, publishes exact `product-extensions` inventory and
  withdraws on shutdown.
- focused warnings-denied clippy across extension-host, extensions, skills,
  Agent, LLM, MCP, UI, hooks and CLI — green.
- No full-workspace gate is claimed for this focused slice; the final scoped
  program gate follows U22/O05/R05/R08 and the OpenRouter evidence lane.

Hard lesson: GOTCHAS #258.

## 2026-08-30 — U22/O05/R05/R08 accepted; POR04/QLIVE01 evidence lane admitted

Tracker: `U22`, `O05`, `R05` and `R08` move from not-started to complete.
`QLIVE01` moves from not-started to active; `POR04` remains active. Exact
rollup becomes **230 complete · 29 active · 33 not started · 0 blocked**;
unfinished reaches **62**. The scheduling view is 8 external evidence/approval,
23 actionable implementation, 2 independent proof/docs, 22 dependency-blocked
and 7 post-beta/optional.

U22 adds `UiSlot::SidePanel` and exact Diff/Jobs/Agents contributions. Persisted
`cycle-side-panel` ships on Ctrl+B and cycles the three views before closing.
Diff consumes committed edit/write transcript results, Jobs consumes the
effect-owned registry and Agents consumes provider/preset descriptors without
cross-owner child ids. Full TUI and screen-reader modes use the same bounded
control-free snapshot, and every higher-priority modal continues to preempt it.

O05 makes background `task` a durable lifecycle operation. `JobRegistry::spawn`
reserves the stable id/token before the future can run and retains the
JoinHandle through settlement/disposal. Completion, failure and cancellation
reserve one settlement, append the exact FollowUp inbox notice, then publish
the terminal row; append failure restores the wake budget and running state.
Cancellation reaches the child token. A failed handle attachment cancels,
aborts and removes the reservation. Separate `subagent-jobs` attaches weak
Agent/job handles after Agent composition rather than creating a lower-layer
dependency cycle.

R05/R08 use one `RuntimeSubagentProvider` over the existing delegated-runtime
registry. It admits only permission-capable delegated runtimes, creates a fresh
one-shot durable child, appends `runtime/linked`, applies R02 directly, logs
bounded commentary/reasoning/final/usage and sends parent policy decisions only
as AllowOnce or Deny. Interactive questions fail/cancel instead of receiving
invented answers. Caller cancellation calls the runtime and every outcome waits
for quiescent close.

Current installed-protocol work was necessary for real acceptance. Codex's
Homebrew-style shim is selected from the sanitized PATH before its canonical
script/interpreter identity is bound, and ephemeral start sets the pinned
app-server flag. Claude's current long-lived stream uses the official SDK
control initialize and content-array user frames, lazy init, empty initial
session id and exact `{"mcpServers":{}}`; TurnStarted precedes the write.
Ephemeral mode combines no-session-persistence, prompt-history suppression,
host identity and disabled MCP/Chrome/slash-command surfaces. Explicit
tool-free installed subscription canaries passed against Codex CLI **0.146.0**
and Claude Code **2.1.251** without inspecting credential values. Deterministic
fixtures separately prove plan/tool, allow/deny, cancellation and durable final
behavior.

QLIVE01 contributes `.github/workflows/openrouter-glm-live.yml` plus an
explicitly gated production-composition test. It requires `HEYCODE_E2E=1` and a
process-scoped OpenRouter credential, forces the live
`z-ai/glm-5.3-flash` catalog, verifies reasoning/tools evidence, runs a fixed
text turn and exactly-once client-tool loop, inspects durable reasoning/tool
headers and writes only content-withheld closed metadata. `LiveArtifact` now
stores a wall-clock instant and rejects both future and expired passes. Missing
credentials write a skipped artifact and fail the lane.

This host has no process-scoped OpenRouter credential and retains the known
dummy Keychain shadow. No authenticated OpenRouter request was made, so POR04
and QLIVE01 remain active. Public catalog evidence and a checked-in workflow do
not satisfy their live acceptance (GOTCHAS #259–#261).

Final local evidence is green: `cargo fmt --all --check`, warnings-denied full
workspace clippy and `cargo test --workspace --no-fail-fast` pass at **3,360
unit/integration tests plus 6 doctests**, zero failures/ignored. The isolated
real binary doctor reports healthy with 4 pass and no warning/fail/skip; the
isolated fake headless turn returns `[done:stop]`. The documentation gate
regenerated 4 fresh references, checked 55 links, syntax-checked 16 shell
blocks, executed 1 deterministic example and audited 3 accessible diagrams.

## 2026-08-31 — Q14/Q15 production release-manager bridge; live rows remain active

Tracker status is unchanged: Q14, Q15 and Q16 stay active and the exact rollup
remains **230 complete · 29 active · 33 not started · 0 blocked**. This slice
closes the named implementation gaps without converting unobserved release or
fresh-machine evidence into checkmarks.

`heycode-install` now owns an effect-ready `ReleaseManager` above its authenticated
lower boundary. `ReleaseBundle` reads four explicit non-symlink regular files
under pre-allocation bounds and owns the bytes through verification/mutation.
Fresh installs now obey stable/preview/pinned and enabled-plugin API policy,
not just updates. Apply authenticates the manifest, evaluates policy, verifies
the exact artifact and then invokes the existing journaled publication;
rollback re-verifies every retained proof and remains directional. Held manager
handles become terminal on Context shutdown.

Production plugin `release-manager-gh` injects `subprocess`, resolves the
GitHub CLI through that provider and uses one joined private-runtime worker per
sync verifier call. Subject/bundle bytes live in an owner-only temporary
directory, argv/environment/output/deadline are bounded, inherited environment
is cleared, only isolated HOME/cache/config values are supplied, and
stdout/stderr never reach diagnostics. Verification pins exact
repository, signer workflow, GitHub OIDC issuer, offline bundle and an explicit
normalized `refs/tags/...` source ref. The release workflow now refuses to sign
unless `GITHUB_REF == refs/tags/v<version>`.

`heycode release apply|rollback` is the management surface. It composes only an
explicit off sandbox, the ordinary local subprocess provider and the release
manager—no provider/session/TUI world. Immediately before each operation it
loads current lifecycle state and resolves every enabled installed manifest
into a deterministic `PluginCompatibilitySet`. Apply takes explicit absolute
manifest/bundle/artifact paths plus trust/channel inputs; rollback supplies the
current config classification and re-snapshots plugin compatibility. Service
key `release-manager` raises the exact built-in registry to 64.

The native release workflow now uses the shipping apply command before either
onboarding smoke and requires the dispatch ref to equal the version tag. A
cross-platform review caught that `InstallRoot` used extensionless `bin/heycode`
on Windows; it now publishes `heycode.exe` there and the workflow executes the
exact returned stable path. Each OS runs an isolated deterministic fake turn
and a second credentialed GLM-5.3-Flash turn. Strict schema-v1 evidence parsing
rejects unknown/future/incomplete/zero-run data, and a final
`heycode release evidence` job succeeds only for three admitted real-provider
artifacts. These are still definitions until hosted execution occurs.

Focused install evidence: manager tests first failed on the missing product
types; verifier and service tests first failed on their missing boundaries.
An additional red regression proved manager construction created the install
record before authentication; lazy root publication fixed it. After
implementation, all **64 heycode-install tests** and all-target
warnings-denied clippy pass. Once the UI lane restored a coherent checkpoint,
all **6** release-surface CLI tests and the outer binary parser test passed;
the earlier compile stop was correctly attributed to the known TUI mid-edit,
not retried blindly. Unix script syntax, YAML parsing and one local deterministic
fresh-machine smoke pass; PowerShell is unavailable on this host and is not
claimed. CLI all-target `--no-deps` warnings-denied clippy is green; full
dependency clippy remains a later root milestone after concurrent lanes settle.
Linux and Windows GNU target checks for heycode-install pass (dependency warnings
on the Windows cross-check remain separate lane work). No actual heycode Sigstore
bundle or three-OS real-provider run exists
yet, so Q14/Q15/Q16 remain active (GOTCHAS #262/#263).

A safe installed GitHub CLI 2.87.0 product probe also passed its construction
boundary: `heycode release apply` composed `release-manager-gh`, successfully ran
the bounded `attestation verify --help` capability check, then stopped at the
intentionally absent manifest with only the fixed `manifest unavailable`
diagnostic. It did not read a credential, fetch an attestation or claim a
signature pass.

That probe exposed verifier scratch under `<install>/.verification`, which
still created the requested root before bundle admission despite the manager's
lazy open. A red product regression now proves missing input leaves the install
path absent, and the CLI keeps verifier scratch in an enclosing ephemeral OS
directory instead. The exact regression passes. A later repeat against the
installed GitHub CLI was deferred when the concurrent job lane temporarily
left heycode-exec uncompilable; no repeated probe is claimed while that known
mid-edit blocker exists (GOTCHAS #265).

## 2026-08-31 — PAWS04–06 provider-local closure; shared/live rows remain active

Tracker status and rollup are unchanged. The exclusive AWS lane completed the
provider-local portion at **130 heycode-provider-aws + 50 authorization-aws
tests**, focused formatting and warnings-denied clippy. Hosted branches did not
run because the required key, region and model selectors were absent; no
credential value was inspected or printed.

Converse now retains an exact operation credential reference, resolves once per
operation, observes rotation on the next call, gates streaming on affirmative
discovery and owns a credential-safe hosted canary. Mantle Responses and
Messages have distinct paths/auth/protocol profiles, reject currently
incompatible model families before transport, fail invalid defaults at
construction and preserve caller cancellation. Runtime metadata binds cache
placement/count/TTL evidence to the selected canonical model, validates
guardrail configuration without echo and classifies foundation/geographic/
global/application inference targets exactly.

PAWS04–06 stay active. Root must still add the model-aware shared provider-option
hook; serialize cache points/guardrails; preserve read/write/TTL and lossless
reasoning state through normalized events/session/usage; register mutually
exclusive Bedrock/Mantle factories and Settings; and run authorized hosted
canaries. The lane correctly reported those owners rather than promoting
provider-local tests (GOTCHAS #264).

## 2026-08-31 — PGCP05–07 provider-owned closure; shared/live rows remain active

Tracker status and rollup are unchanged. The exclusive Google lane passes
formatting, warnings-denied clippy and all **117 heycode-provider-google tests**.
Authorization-GCP was intentionally unchanged: ADC account, project and
location remain three tokenless independent verdicts.

PGCP05 now represents Google Search and Vertex external-API grounding as
distinct provider requests. External API auth can be absent or a Secret Manager
resource reference; the boundary cannot hold an API-key value. Public
`retrievedContext` URLs produce durable-ready citations while private query/
snippet material stays excluded. Unique excerpts retain exact spans; repeated
or missing excerpts degrade to unanchored sources instead of guessing.

PGCP06 retains code-execution call/result correlation, opaque provider ids,
closed failure classes, explicit/implicit cache provenance, absent-versus-zero
usage and modality detail. PGCP07 pins current Sonnet 5 thinking/effort choices
and a strict gated Messages probe with ordered settlement, signed/redacted
thinking, forced tool input, event-name correlation and distinct cancellation.

All three rows remain active. Root/shared work still includes capability
vocabulary, Agent native-route selection, Gemini/Vertex option serialization
and response events, durable cache usage, a data-driven Vertex Messages
dialect, factories/schema/credential provider and authorized global-region
canaries (GOTCHAS #266).

## 2026-08-31 — PL09–PL11 lower code/WASI/inspection boundaries active

`PL10` and `PL11` move from not-started to active; PL09 remains active. Exact
rollup becomes **230 complete · 31 active · 31 not started · 0 blocked** and
unfinished remains 62. No row is accepted because each still names a missing
product/security owner.

PL09 now prepares and commits one six-domain code generation in two phases.
All concrete domain adapters must accept before proxies become available;
partial refusal unwinds in LIFO order. Host refusal, process crash and caller
cancellation retire the complete generation, and a response racing retirement
cannot escape. An ordinary remote invocation denial returns a denial without
destroying an otherwise healthy process.

PL10 adds exact WIT v1 and Component Model boundary types. They bind component
and package bytes independently, reject dylibs and core Wasm modules, pin the
current WASI 0.3.1 import contract, default to no ambient authority and require
explicit preopens/endpoints plus an exact engine ABI report. PL11 adds a bounded
curated projection with no fields for descriptions/bodies/paths/locators/
digests/runtime ids/errors/settings/credential references. No model tool is
registered while QSEC01 remains unapproved.

Focused evidence is extensions **6 unit + 129 integration** and extension-host
**3 integration** tests, including 9 protocol and 2 product-host PL09 tests.
Formatting, extensions warnings-denied clippy and extension-host `--no-deps`
clippy pass. Rows remain active until heycode-exec supplies byte-bound capability-
scoped process ownership, a real Component engine typechecks/executes WIT under
an empty-default WASI context, root supplies managed package/grant/session plus
six real adapters, and QSEC01 authorizes any inspector exposure (GOTCHAS #267).

## 2026-08-31 — U17/U21/CMD09/CMD10 accepted

The four rows move from not-started through active to complete. Exact rollup
becomes **234 complete · 31 active · 27 not started · 0 blocked**; unfinished
falls to 58.

The durable transcript reducer now correlates multiple tool/provider events and
renders provider state, server tools/usage, citations, compaction, runtime link,
route and plan state through shared full/flat projections. Item-level bounded
caching walks only the requested viewport. Its 100K-event bottom/middle test
finishes locally in 0.05–0.07 seconds against explicit replay <2s and render
<1s budgets, renders at most 40 new items for a middle frame and retains at
most 256 fragments.

CMD09 adds human-only diff/copy/mention plus explicitly model-scheduling review;
only review enters durable model history. Copy uses the terminal OSC52 boundary.
CMD10 adds live/persisted theme, keymap and Vim controls through Settings CAS,
with `keymap` and `ui-preferences` namespaces. Owned evidence is TUI **179** and
UI **55** tests, clean diff and no-deps warnings-denied clippy. Root updated the
exact default command/settings inventory; both real-composition and verbose
plugin-attribution tests pass. The documentation gate then regenerated four
fresh references, validated 55 links and 16 shell examples, executed its one
deterministic example and audited three accessible diagrams. That joins
behavior, reachability and durable documentation, so all four rows are accepted
(GOTCHAS #268).

## 2026-08-31 — MCP11/MCP13/PMM04/PZA05/O09 lower boundaries; rows remain active

Tracker status and rollup are unchanged. The MCP lane passes **337 MCP**, **21
hooks**, **126 MiniMax plus 4 doctests**, and **68 Z.AI plus 2 doctests**, with
formatting and owned warnings-denied gates green. No MiniMax/Z.AI server or
provider credential was contacted.

MCP11 routes elicitation, progress and logging by exact session/client identity
with bounded payloads, cancellation and panic containment across stdio and
finite HTTP/SSE evidence. MCP13 filters generated tools by exact allowlist/deny
policy and invokes action-time approval without making server annotations
available to that decision. Bound stdio/HTTP servers retain credential queries
and resolve only when the operation starts.

MiniMax's Token Plan bundle is an effect-owned ordinary MCP host. Z.AI specs now
carry exact credential queries and explicit optional/Prompt/no-exposure policy.
O09 supplies typed prompt/subagent/MCP hook ports with provenance and failure
policy. These are not yet product completion: root must supply configured
approval/broker/client-route/human-event owners, register provider factories,
admit Z.AI's Node/package identity, invoke hooks at real lifecycle points and
create a durable session event before any hook output enters model context.
Streamable HTTP still needs a duplex exchange capable of posting an elicitation
reply before the response stream closes. The lane continues into MCP15 official
Inspector/OAuth evidence; all five implementation rows stay active
(GOTCHAS #269).

## 2026-08-31 — POA03/PAN03/PDS04/POR05 shared protocol closure; rows remain active

Tracker status and rollup are unchanged. The shared/provider lane passes **410
LLM, 62 OpenAI, 81 Anthropic, 79 DeepSeek and 47 OpenRouter tests**—679 total—
plus formatting, warnings-denied focused clippy and clean diff. No authenticated
provider call ran.

Responses and Messages now carry exact hosted/server-tool plans through
transport, normalized call/result/citation events, cross-request correlation,
lossless provider state and replay. Anthropic code aliases, MCP restrictions,
deferred tool search, later settlement and pause-turn continuation are exact.
OpenAI classifies/gates all seven families; web/file/code/hosted-shell cross the
full route, while computer/image/approval-bearing MCP remain inactive pending
their upper owners rather than emitting synthetic calls.

DeepSeek adds a guarded Anthropic-format adapter with operation credentials,
silent model-substitution refusal, exact route descriptor and a gated two-leg
live smoke. No credential existed. OpenRouter normalizes citations and both
documented aggregate search-usage shapes, including repeated usage-frame finish
reasons, but still has no per-search ids/results.

All four rows remain active. Root/shared work still includes request-specific
N01/P10 option selection so static all-tool plans cannot bypass prefer-local,
factory/dialect activation and computer/image/MCP upper bridges. PDS04 and
POR05 also keep their explicit authenticated evidence acceptances (GOTCHAS
#270). The same thread continues into the AWS/Google shared-cloud option/state
bridges now that those provider-local lanes are idle.

## 2026-08-31 — R10/R12 production runtime plugins composed; live acceptances remain

`R12` moves from not-started to active; `R10` remains active. Exact rollup
becomes **234 complete · 32 active · 26 not started · 0 blocked**. The workspace
now contains **59 crates**, and the current Unix host composes **102 default
plugins** after availability filtering. The older 3,360-test milestone remains
the last workspace-wide gate; these changes have only focused evidence while
the other saved-project tasks are still editing the shared tree.

New crate/plugin `heycode-runtime-opencode` adapts the provider-neutral ACP owner
to the composed raw-interactive subprocess service. It binds the exact OpenCode
1.18.21 executable hash, version and initialized `agentInfo`, supplies a sorted
explicit environment, keeps runtime-generation and per-operation cancellation
separate, and exposes a truthful Unavailable row when the optional installation
is absent. A credential-blind installed canary passed exact version,
initialize, session-scoped model catalog and quiescent close under isolated
HOME/XDG roots. It did not send a model turn, so R10's authenticated GLM
acceptance remains open.

New crate/plugin `heycode-runtime-deepseek-harness` pins SDK server identity and
protocol 0.0.1 at the reviewed Harness commit. Its strict closed union retains
root/child sequence identity, prompt receipt/status/turn/tool/cache-aware usage
and final settlement; cancellation reaps the complete child because that wire
has no prompt-cancel method. A local-source canary was attempted, but the pinned
checkout contained no built `dsh-jsonrpc-agent`/`lib/bin.js` or usable installed
`tsx` payload, so no live delegation is claimed and R12 stays active.

Focused evidence is heycode-runtime **21**, heycode-runtime-opencode **7** and
heycode-runtime-deepseek-harness **7** tests with owned formatting and
warnings-denied clippy. Root added both factories/default rows and passed the
exact real-composition inventory plus verbose attribution checks. Registry ids
are sorted while dynamic inventory follows registration order, so the tests
compare those distinct contracts explicitly rather than assuming one order for
both (GOTCHAS #271).

## 2026-08-31 — E09/CMD08/O10–O13/CMD11 accepted as one operational vertical

`E09`, `CMD08`, `O10`, `O11`, `O12`, `O13` and `CMD11` move from not-started
to complete. Exact rollup becomes **241 complete · 32 active · 19 not started ·
0 blocked**; unfinished falls from 58 to 51. No downstream row becomes newly
dependency-ready beyond this accepted group.

`execution-jobs` binds the already composed shell and terminal owners to the
Agent JobRegistry. Shell defaults resolve before reservation; PTY work uses the
same exact process spec. One waiter owns settlement, cancellation reaches the
whole tree, a concurrent hard stop joins that waiter, and the source-attributed
inbox notice commits before visible terminal state. Model tools
`background_shell|background_terminal` and human-only `/tasks|ps|stop` consume
that same service.

`goals` supplies revisioned CAS snapshots, disarmed-on-resume activation,
bounded rounds and bounded consecutive wakes. `AgentIdle` publishes only after
the turn lease drops, letting the driver checkpoint before reserving one A03
FollowUp. Mid-turn plan changes remain pending until the next accepted pre-step;
the review gate and `/plan <message>` cross that same commit boundary. `/goal`
and the model goal tool share the durable owner.

`workflows` separates its replaceable Provider from the model tool. The default
sequential worker reports progress/checkpoints, cancellation settles before the
job notice, and resume begins strictly after the durable completed prefix.
`schedules` supports delay/absolute/fixed-rate records, flushes before every
read/mutation, enqueues before correlated dispatch, repairs only that crash
window, rebuilds timers on resume and excludes inherited events from a fork
until explicit copy.

Root composed all four optional plugins after `agent`/`subagent-jobs`, added
their four service keys, and extended exact plugin/service/command/tool and
verbose-attribution audits. The current Unix default grows from 102 to 106
plugins and the authoritative service registry from 64 to 68 keys. Config
schema remains v25: these are optional Consumers and no historical plugin
injects their services, so inserting them into an intentional exact profile
would violate profile authority. Profile-free startup derives the live default
and receives them automatically.

Owned evidence is heycode-agent **201** plus heycode-session/heycode-exec **251** tests
(452 total, zero failures), focused formatting, warnings-denied clippy, clean
diff and downstream app-server/status/TUI check. Root then passed the exact
default composition, built-in service-key and `/plugins verbose` attribution
tests plus heycode-cli all-target no-deps warnings-denied clippy. No workspace-wide
gate is claimed while five other tasks still edit the shared tree (GOTCHAS
#272).

## 2026-08-31 — MCP15 accepted with separated Inspector and OAuth evidence

`MCP15` moves from active to complete; `MCP11` remains active. Exact rollup
becomes **242 complete · 31 active · 19 not started · 0 blocked** and unfinished
falls to 50.

An isolated invocation of official `@modelcontextprotocol/inspector@2.4.0`
strict-listed five tools and called the ordered rich-result tool over both a
real local stdio server and a real local Streamable HTTP server. Resources,
prompts, logging and protected-resource initialization also ran. No package was
installed globally and all Inspector/npm state was temporary.

The production McpConnection and McpStreamableHttpClient/Reqwest paths traverse
listings, ordered rich blocks, structured-output conformance, cancellation and
body-free hostile diagnostics. Finite SSE now admits server requests before
processing later notifications: accepted elicitation posts one exact response,
while a later cancellation retires the pending id before handler dispatch and
emits no late reply.

A separate stateful local authorization/resource server proves protected
resource discovery, exact issuer and callback issuer, CSRF state, S256 verifier,
authorization code, token audience, refresh, protected access, cancellation and
canary-free failures. The test retains mandatory semantic HTTPS identities but
uses a custom plaintext loopback transport because the shared HTTP service has
no test-CA injection seam. Inspector initialized with a temporary fixture token;
heycode, not Inspector's browser UI, drove the complete code/PKCE flow. MCP15's
literal local protocol matrix is therefore observed without claiming browser
OAuth, local TLS or a hosted third-party server.

MCP11 remains open: buffered `HttpService::send` cannot service an SSE response
that stays open until it receives the concurrent elicitation reply. That needs
a genuinely streaming response API which branches JSON/SSE while another POST
settles the server request. Focused evidence is **340 heycode-mcp tests** (8 unit +
332 integration), package formatting/diff and warnings-denied no-deps clippy
(GOTCHAS #273).

## 2026-08-31 — Q10/Q11 black-box performance and matched-eval harnesses accepted

`Q10` and `Q11` move from not-started to complete. Exact rollup becomes **244
complete · 31 active · 17 not started · 0 blocked**; unfinished falls to 48.
`QSEC05` remains not-started at this point because its first product run found a
real request-projection defect rather than passing.

The dependency-free quality layer owns private HOME/HEYCODE_HOME/workspace roots,
process groups, bounded output drain, timeouts/descendant cleanup, deterministic
statistics and schema-v1 content-free artifacts. Reports structurally lack
prompt/content/output/command/path/environment/credential/request/response and
reasoning fields; Unix publication is atomic owner-only.

Q10 records five product-facing metrics without importing crate internals:
startup exit, local fake TTFT, resume/replay of 1,000 public v2 events, first
screen-reader frame and 1,000-tool composition. Five repeated local debug runs
pass the enforced CI regression profile with p95 values 35.4, 526.4, 568.8,
562.4 and 559.7 ms respectively. The release-reference targets remain proposed
until root records reference hardware and an accepted content-free baseline;
no release-speed claim is made.

Q11 copies clean fixtures and gives candidate/reference agents identical
model/permission/sandbox/timeout declarations, alternates order, uses
deterministic graders and allowlisted file changes, then emits Wilson 95%
intervals and a fixed-seed paired bootstrap. Both fixture agents pass 9/9
tasks. The report correctly says `insufficient_evidence` because nine pairs are
below its 30-pair decision floor; it does not manufacture superiority or
non-inferiority.

Evidence is **22 Python harness tests**, parsed workflow/JSON fixtures, five
repeated product performance samples, 9/9 matched task results and successful
content-free artifact/canary validation. The optional live comparison remains
manual and credential-gated. QSEC05's negative and positive-control harness is
also present, but its missing model-visible wrapper keeps that separate row open
(GOTCHAS #274).

## 2026-08-31 — QSEC05 strict untrusted projection repaired and accepted

`QSEC05` moves from not-started to complete. Exact rollup becomes **245 complete
· 31 active · 16 not started · 0 blocked**; unfinished falls to 47.

The first real product evaluation correctly failed all three Web/MCP/LSP cases.
Durable events retained the exact typed source and `projected_input` computed
the appropriate model warning, but the `Role::Tool` branch passed
`message.content` rather than the derived `content` into
`ChatMessage::tool_result`. Compatibility mapping and other roles already used
the derived value, which is why nearby behavior tests did not expose the strict
route.

A red unit regression now enumerates every closed untrusted source and asserts
the exact source-specific wrapper, call id and error state at strict inference;
an unlabelled tool result control asserts byte-exact legacy content. The minimal
fix changes the shared Tool sink to consume the derived value, closing plain and
rich results without per-source branches. Manual bypass review checked the
direct Agent/compaction/C05 callers and the independent compatibility mapper;
no alternate strict copy remains.

After rebuilding the real binary, the QSEC05 black-box gate passes three denied
source cases. Each provider request contains the source label, data-only/no-
authority warning and hostile canary; the provider issues a real write request;
deny policy records the failure, creates no marker and both processes settle.
The independent auto-approval control creates its marker, proving the action is
not vacuous. `/tmp/qsec05-dshx-root.json` passes the content-free artifact
validator.

Verification is the two focused Rust boundary tests, `cargo build -p heycode-cli`,
the real-binary three-source/positive-control evaluation, artifact validation,
direct rustfmt checks for the changed Agent/TUI files and warnings-denied
heycode-agent lib clippy. A package-wide fmt check was not claimed because the
concurrent O06/O07/O14 task had an unformatted in-progress test outside this
fix; the changed files themselves are clean (GOTCHAS #275).

## 2026-08-31 — Q13 accepted; Q12 active after ANSI/OSC render repair

`Q13` moves from not-started to complete and `Q12` moves from not-started to
active. Exact rollup becomes **246 complete · 32 active · 14 not started · 0
blocked**; unfinished falls to 46.

The isolated `chaos` package exercises five public settlement boundaries under
one explicit seed: composition apply failure/LIFO rollback, complete/torn/open
session settlement and read-only repair, quiescent subprocess-tree
cancellation, raw SSE fragmentation, and provider error-body redaction plus
status-authoritative classification. Two pre-acceptance runs produced
byte-identical schema-v1 content-free reports, and the final complete local
smoke passed all five again. That satisfies Q13's framework acceptance without
claiming long-duration chaos.

The isolated `fuzz` package owns five bounded libFuzzer targets and twenty
synthetic credential-free corpus seeds for session, config, provider, MCP and
the public full/flat render paths. The first root reproduction of
`ansi-controls.txt` failed deterministically: pulldown-cmark/syntect preserved
ESC colour and OSC 8 hyperlink bytes into ratatui cell symbols. The flat output
sanitizer did not protect the full-screen frame.

The production Markdown boundary now normalizes CRLF/CR to LF, keeps LF
structure and visible text, turns tabs into spaces and replaces every other
control before parsing or syntax highlighting. A focused test preserves the
visible `red`/`label` text while asserting all styled spans are control-free.
The original seed then passes the public draw target.

The one complete local PR smoke passes the five chaos scenarios, 128 fixed-seed
libFuzzer/ASan runs each for session/config/provider/MCP, the 8-run high-cost
render set, isolated package formatting and warnings-denied clippy. Q12 remains
active because neither the scheduled nor manually dispatched hosted
three-minute-per-target matrix has been observed; short local smoke and a
workflow definition cannot satisfy continuous evidence (GOTCHAS #276).

## 2026-08-31 — X07 installed VS Code extension and shipping stdio host accepted

`X07` moves from active to complete. Exact rollup becomes **247 complete · 31
active · 14 not started · 0 blocked**; unfinished falls to 45.

The app-server crate now exposes a bounded multiplexed NDJSON transport around
unchanged v1 request/notification/response objects. One outer safe-integer
operation id permits a long turn to coexist with permission and cancel
requests. Duplicate/mismatched correlations, malformed input and EOF settle all
admitted work; the sole writer is joined and raw frames are never logged.

Root added the shipping CLI surface `heycode app-server --stdio-v1 --workspace
<absolute-path> [--resume <session-id>]`. Its closed parser keeps global
config/profile/provider/model/fake/trust choices outside the subcommand,
canonicalizes an existing directory and admits only canonical UUID session ids
before joining the sessions root. Without an explicit flag it chooses
restricted workspace authority. The normal world supplies AppServer; stdout has
no UI listener; EOF/Ctrl+C settles the transport and Context then unwinds LIFO.
A red real-binary test first observed the missing command, then passed
initialize/open/fake-turn/close with every stdout line valid protocol JSON.

The installable `heycode.heycode-vscode@0.1.0` package uses the fixture-locked
TypeScript SDK, starts one exact executable with `shell:false`, drains/discards
stderr and exposes six commands. The final isolated installed Extension
Development Host used `target/debug/heycode --restricted-workspace --fake
app-server --stdio-v1`, opened a native session, observed active work before an
overlapping cancel, settled cancel, closed, resumed the exact UUID in a fresh
host, completed a healthy turn and disconnected. The shipping fake emitted no
permission/question requests; the 7/7 extension protocol suite separately
proves exact allow-once and deny correlation.

Evidence is app-server **14**, Rust SDK **5**, TypeScript SDK **4**, extension
protocol **7**, root parser **2** and shipping-binary **1** focused tests;
warnings-denied Rust/TypeScript checks, VSIX packaging (seven files, 15.34 KiB),
isolated install, Extension Development Host activation and the installed
journey all pass. A second turn on the same finite fake host returns unavailable
because main deliberately supplies one scripted response; close+fresh-process
exact resume is healthy, so that observation is not a RuntimeSession regression
(GOTCHAS #277).

## 2026-08-31 — AWS/Google request-specific shared bridges complete; rows remain active

Tracker status and rollup are unchanged. Focused evidence is **419 heycode-llm,
131 heycode-provider-aws and 124 heycode-provider-google tests** (674 total), package
formatting/diff and warnings-denied clippy. No cloud credential was read and no
hosted request ran.

`ProviderOptionContext` now carries the selected model and exact N01 routes.
Provider wrappers can materialize options for that request rather than exposing
static all-request defaults. Bedrock Converse derives and independently
validates cache placement/TTL, guardrail and source/target/cross-region route
data before wire serialization. Read/write/TTL cache details normalize into
the neutral detailed response without adding per-TTL values on top of the
aggregate write count.

Gemini/Vertex accepts only plans whose provider, selected model, route and
native evidence agree. Google Search, external API grounding, code execution
and explicit cache choices cross request and normalized event/state paths;
configured but unselected features remain absent. `ClaudeVertexProvider`
resolves an operation bearer credential and configures the shared Messages
adapter with an exact endpoint/model dialect plus
`anthropic_version=vertex-2023-10-16`, reusing existing tool/thinking/state
validation rather than another parser.

PAWS04–06 and PGCP05–07 remain active. Agent still must call
`request_options_for` after N01 selection and before P10/header commit. Core has
no `BedrockConverseMessage` provider-state kind, so opaque reasoning/redacted
content continues to fail safely rather than becoming lossy replay. Root also
owes provider-native candidate registration, explicit mutually exclusive
factories/settings/credentials and the gated hosted canaries. Shared adapter
tests are neither product reachability nor live evidence (GOTCHAS #278).

## 2026-08-31 — MCP11 duplex transport closed; product route remains active

Tracker status and rollup are unchanged. MCP11's genuine open-stream transport
gap is closed, but the row remains active because its real Agent/TUI route,
human broker, progress/log sink and approval owner are not yet attached.

`heycode-http` now exposes a pull-owned dynamic response whose validated status and
headers are available before body completion. One caller pulls bounded chunks
under its cancellation token; cumulative caps and backpressure are explicit;
body/header values stay out of diagnostics; no background drain task exists.
The ordinary buffered transport remains compatible through a one-chunk adapter.

MCP Streamable HTTP consumes that body incrementally and owns concurrent
elicitation handler/reply POST futures in a local `FuturesUnordered`. The real
fixture holds the original SSE open until its reply arrives, then emits
progress, logging and the final result. Cancellation retires the exact pending
request and sends no late reply. This is protocol/transport evidence, not the
missing product session route.

MiniMax now requires canonical executable/cwd admission before its Token Plan
bundle can convert to a launch. Z.AI vision additionally requires Node >=22,
package >=0.1.2 and pins the observed package version in argv rather than using
`@latest`. Credential queries remain unresolved operation-time data. Neither
boundary is a bound MCP generation or live tool discovery, so PMM04 and PZA05
remain active. O09 also retains its Agent lifecycle/durable-input work.

Focused gates: heycode-http **47**, heycode-mcp **341**, MiniMax **127 plus 4
doctests**, Z.AI **69 plus 2 doctests**, hooks **21**, with formatting and
warnings-denied all-target clippy across all five crates (GOTCHAS #279).

## 2026-08-31 — PLM05/R10/R12 local gates strengthened; no live turn claimed

Tracker status and rollup are unchanged. PLM05, R10 and R12 remain active
because this host cannot produce their literal model-turn evidence safely.

PLM05 gains a five-surface, no-tools, content-withheld Ollama Chat gate which
never starts a daemon or pulls a model. The host has no `ollama` executable and
no response on port 11434, so the gate does not run and no local-model claim is
made.

OpenCode 1.18.21 is bound to SHA-256
`8c783005340f8dfc5e7d168478dd0dd2bd1faead531cb34270de2a9689d9f135`.
Its credential-blind ACP version/initialize/session catalog/quiescent close
canary passes. The installed catalog does not expose current official id
`opencode-go/glm-5.3-flash`; fixtures and the live gate use only that id, deny
all tools and withhold content, so no fallback to the historical Ox alias and
no model turn occur.

The Harness checkout remains pinned to
`528c682e061696f5a160f363f236ecbf53cbd006`. Runtime admission now hashes the
launcher and explicitly registered reviewed artifacts. The checkout lacks its
built server and `tsx`, requires pnpm 11.7.0 while this host has 9.15.0, and its
lockfile is already materially modified. Building would mutate an unreviewed
state, so the canary stops rather than manufacturing an artifact.

Focused evidence is LM Studio/Ollama **111**, runtime **21**, OpenCode **8** and
Harness **9** tests (149 total), owned formatting and warnings-denied no-deps
clippy. Live-gated branches are not counted as observations (GOTCHAS #280).

## 2026-08-31 — O07/O14 product-composed; O06 active pending explicit base

`O07` and `O14` move from not-started to complete; `O06` moves from not-started
to active. Exact rollup becomes **249 complete · 32 active · 11 not started · 0
blocked**; unfinished falls to 43.

O06 adds `GitWorktreeManager` and a delegated-runtime subagent Provider. Every
Git operation uses the composed subprocess service, exact argv, empty
environment, disabled hooks/fsmonitor/credential helpers, full lowercase
commit ids, journal-before-side-effect transitions and synchronous cleanup or
explicit failure retention/recovery. Its focused lifecycle/provider tests pass.
Root does not activate it by default: no product config currently owns the
required exact base, and silently resolving mutable HEAD outside the composed
process boundary would weaken its contract.

O07 adds closed v2 `team/change` truth plus default service/tool `teams`/`team`.
Root authority creates rosters, continuable-child membership, task DAG and
dispatch; children see only authorized peer mail. Global and task-local CAS,
cycle/dependency/ownership checks, bounded waiters and explicit blocked crash
recovery are durable. Dispatch commits in-progress before child input and
terminal team state before JobRegistry settlement and the A03 follow-up.

O14 adds closed v2 `review/change` plus default service/command `reviews` and
`/review-runtime <runtime> [instructions]`. It captures current tracked base
and patch through the composed Git owner, commits full input before work,
applies it to an isolated checkout, runs only a permission-capable delegated
runtime under DenyAll, validates strict structured findings and requires the
post-run Git status to equal its baseline. Mutation/failure/cancellation commit
closed failure and no findings. The existing TUI `/review` keeps its distinct
active-route prompt semantics; the selectable isolated path is explicit.

Owned gates are **211 Agent + 173 Session tests**, formatting, warnings-denied
all-target clippy and clean diff. Root added both service keys/factories/default
rows and passed exact default composition, built-in service registry, command/
tool catalog, verbose attribution and CLI no-deps warnings-denied clippy. The
default Unix profile grows from 106 to 108 plugins and the service registry from
68 to 70 keys (GOTCHAS #281).

## 2026-08-31 — O06 explicit-base product configuration accepted

`O06` moves from active to complete. Exact rollup becomes **250 complete · 31
active · 11 not started · 0 blocked**; unfinished falls to 42.

Root config schema advances from v25 to v26 and adds optional
`subagent.worktree_base`. The value must be a full lowercase SHA-1/SHA-256
commit and is converted to `GitCommitId` while the factory table is built,
before any plugin effect. An absent value registers no worktree factory; no
branch/ref/HEAD fallback exists. Older v25 documents upgrade without adding a
field, while the saved matrix now covers v0 through v26 and refuses v27
unchanged—an older schema-25 reader therefore cannot silently ignore isolation
intent.

Presence conditionally registers three exact provider factories:
`worktree-codex`, `worktree-claude` and `worktree-opencode`, each with a
separate owner-state root disjoint from the source repository and the same
validated exact base. DeepSeek Harness is omitted because its runtime
descriptor cannot prove permission callbacks. Plugin inventory remains absent
when the setting is absent, preserving default worlds and exact-profile intent.

A real temporary Git repository composition creates a commit, supplies it
through typed Config and proves all three runtime-specific provider/plugin rows
through the production factory/loader; Context shutdown disposes their manager
generations. Focused evidence also includes the existing O06 exact checkout,
cleanup/retention/orphan-recovery suite, schema parse/patch test, every-version
migration matrix and newer-version refusal. CLI focused count becomes 131;
config test count is unchanged because the matrix/test cases were extended in
place (GOTCHAS #282).

## 2026-08-31 — contextual provider admission, Bedrock state and Google baseline composed

Tracker status and rollup are unchanged because PAWS04–06 and PGCP05–07 retain
their native/Vertex/hosted acceptance clauses.

Agent now invokes `Provider::request_options_for` only after catalog alias
resolution and exact N01 selection, then passes those options through P10 and
the durable request header/C05 gate. A red adapter test first observed zero
context calls; the green control receives the canonical model plus complete
client/provider route set and persists the resulting option. Unsupported or
unproven policy remains a preparation failure.

Core adds `BedrockConverseMessage`: a complete assistant role with a nonempty
ordered union of text, reasoningContent and toolUse blocks. The Converse parser
buffers text, tool JSON, opaque signatures and redacted content and publishes
one state only alongside successful terminal metadata/usage/finish. Exact route
replay serializes it verbatim; wrong state or broken tool chronology fails
before transport. Session append/open/project preserves it and treats it as the
authoritative assistant turn. `ProviderStateItem::Debug` now redacts data for
every protocol after the first new test exposed an opaque signature in a test
failure.

The composition root makes the maintained Google Developer default
`gemini-3.7-flash` a provider-owned production route. `llm` publishes the empty
registry/selection/interception service without claiming `google`; later plugin
`inference-google-gemini` registers strict GenerateContent with the configured
operation-time credential and explicit empty native/cache policy. Custom Google
models fail without maintained composition evidence. A real composition test
uses a custom reference, performs zero network/secret resolution, proves exact
inventory/auth preview and proves disposal.

Focused evidence includes the 60-test Bedrock protocol suite, core Bedrock
shape/Debug tests, session provider-state reopen, Agent contextual-option test
and Google production-composition test. AWS factory/settings/hosted and Google
native/Vertex/Claude/hosted rows remain active (GOTCHAS #283).

## 2026-08-31 — PL09/PL10 real process and WASI lower hosts integrated

Tracker status and rollup are unchanged. PL09 and PL10 remain active at the
product activation boundary; PL11 remains active and unexposed behind QSEC01.

PL09 copies verified plugin bytes to a private exact image whose lease is held
by the process handle through terminal settlement, closing a real spawn-return
before-exec deletion race. `HeycodeExecCodePluginLauncher` uses the common sandbox
and process tree with empty environment, bounded length-prefixed fragments,
serialized ids, cancellation, descendant reap and one atomic six-domain
retirement generation.

PL10 supplies `WasmtimeWasiComponentEngine` on pinned Wasmtime 48.0.1/WASIp3.
It typechecks real Component binaries against the WIT import subset, defaults
WASI to no authority, admits only explicit preopens and IP endpoints, refuses
DNS/write-only authority and applies fuel/epoch/table/memory ceilings per Store.
Root moved the exact Wasmtime definitions into workspace dependencies and the
extension-host consumes them with `workspace = true`; Cargo.lock already
contains the pinned graph.

Focused gates are exec **88**, extensions **135** and extension-host **13**
tests (236 total), formatting and warnings-denied clippy. Product activation
still needs an authoritative owner for installed package entrypoint/runtime,
grants/resources/session and six concrete adapters; current root facts cannot
infer them safely (GOTCHAS #284).

## 2026-08-31 — final parallel wave and root integration

Tracker moves ATT04 to complete and POR07/MCP11/MCP13 to active. Rollup becomes
251 complete, 34 active, 7 not started. No live/platform/approval row is
promoted.

### Provider and request preparation

- OpenAI owns an explicit bridge-complete Search/code/shell policy and matching
  `native-openai` effects; Anthropic owns an exact atomic Search/code plan and
  `native-anthropic`. Root configures the Provider and candidate plugin from
  the same literal set. Provider gates: OpenAI 64, Anthropic 84, DeepSeek 80.
- Schema 27 adds `llm.protocol` and optional positive
  `llm.max_output_tokens`. DeepSeek Chat versus Anthropic Messages is explicit;
  Bedrock Mantle requires Responses or Messages and Messages requires the cap.
- `Provider::prepare_inference` is awaited after catalog/model/N01 and before
  adapter/options/P10/C02/C05. Agent cancellation joins the future. AWS uses it
  to force private live evidence; Vertex resolves the composed GCP profile and
  returns an endpoint-bound operation provider. Provider gates: AWS 140, Google
  136. Maintained Vertex catalogs keep account access Unknown.
- Google Developer production enables exact Search and code-execution N01
  candidates. Implicit cache activation was rejected and removed because the
  live catalog reports prompt-cache support Unknown.

### Media, MCP and extensions

- ATT04 validates PCM-WAV bytes/duration/rate/channels/depth, keeps audio on a
  hidden exact adapter plane, commits input/output associations in order and
  exposes only metadata through session/app-server/Rust+TypeScript SDK/TUI.
  Focused Rust evidence is 1,096 tests plus 5 TypeScript tests/build/example.
- MCP stdio no longer discards elicitation handles: the driver cancels/drains
  its future set and late human events are inert. Hook contributions are
  pending until a durable bridge mints a renderable type. MCP 343, Hooks 22,
  MiniMax 127 and Z.AI 69 pass; real Agent/TUI/bound-provider activation stays
  active.
- Installed code activation now has typed manifest runtime/entrypoint,
  managed provenance/session/grants/resources, six real adapters and
  import-exact Wasmtime reporting. Extensions 137 and extension-host 19 pass.
  Root uses the code-aware factory with zero authorities so code fails closed.

### Platform, release and quality

- Reqwest pinned web fetches disable ambient proxies; public URL values reject
  local/metadata targets; sandbox roots reject lossy encodings; Windows path
  vocabulary and Job Object evidence are stronger. Exec 89, sandbox 51 and web
  48 pass. Native Seatbelt and real Linux Docker bwrap pass; hosted Landlock,
  Windows and GitHub workflow observations remain open.
- All dedicated platform workflows now invoke `--test main it::<module>` rather
  than nonexistent binaries.
- The verified release candidate now installs itself and onboarding runs the
  exact stable path. heycode-install 66 and one opt-in real-built-artifact
  transaction pass; fixture signatures are not attestation evidence.
- Root corrected canonical reviewer storage when configured state lives inside
  the repository, regenerated the capability reference and app-server audio
  fixture, and moved the default Unix graph to 112 plugins.

Workspace formatting and all-target warnings-denied clippy are green. The first
full test run found over-eager Gemini cache evidence and one transient TUI
delete stress failure. Cache activation was removed because evidence was
Unknown; the isolated deletion contract passed. The final post-documentation
workspace rerun passes **3,701 unit/integration tests plus 7 doctests** (3,708
executed, 0 failed, 0 ignored).

Final isolated shipping-binary evidence uses one Python-owned temporary root:
doctor is healthy with four checks, the fake headless turn contains
`[done:stop]`, and raw stdio app-server v1 completes initialize → session/open
→ turn/start (`stop`) → session/close with protocol-exclusive stdout. The docs
gate reports four generated references fresh, 55 links valid, 16 shell blocks
syntax-valid, one deterministic example and three accessible diagrams; a final
source-digest regeneration check is clean.

The metadata-only setup composition now registers five provider-owned profiles
and catalogs—Anthropic, DeepSeek, Google Developer, OpenAI and OpenRouter—while
retaining zero inference/session/tool/TUI/MCP construction. This extends U10
discoverability without a binary provider/model table or an invented AWS/
Vertex account default.

## 2026-08-31 — MCP product attachment and Settings-backed provider activation

Tracker promotes exactly PAWS05, MCP11 and MCP13. The rollup is **254 complete,
31 active, 7 not started, 0 blocked**; unfinished falls from 41 to 38 and
actionable implementation from 11 to 8. O09, PMM04/PZA05, PAWS06,
PGCP05/PGCP06 and PL09/PL10 retain their concrete handler/provider/live or
managed-generation gaps.

### MCP, hooks and durable identity

- Fresh composition mints one `SessionId` before apply and passes it to
  create-new/no-clobber session storage plus every configured MCP connection
  router. Resume derives the existing directory identity while `Session::open`
  remains the authoritative path/log validator.
- MCP now composes `mcp_product_plugin` with exact one-to-one server routers,
  form+URL elicitation, progress/log sinks, pull-owned duplex replies and the
  ordinary Agent approval policy. Interactive approval is activation-rebound to
  the Context event bus, so Agent, MCP and TUI share one waiter generation.
- `product-hook-attachments` mounts after hooks/session/subagents/Agent and
  before TUI. Prompt/subagent/MCP lifecycle ports commit, flush and reopen the
  new v2-only `hook/contribution` before model projection. No concrete handler
  is inferred from prose, so O09 remains active.
- Provider-bound stdio/bearer-HTTP MCP launch builders retain references only.
  MiniMax entitlement and Z.AI Node/package admission still prevent PMM04/
  PZA05 promotion.

### Provider policy and managed authority

- OpenAI hosted-tool and Anthropic server-tool Settings now drive both provider
  options and N01 candidates. AWS Converse and Google Developer/Vertex use
  activation-time wrappers over the single registered Settings snapshot;
  Google contributes only configured dynamic native rows.
- Schema 28 repairs exact-profile dependencies only for the selected policy
  Consumer. `openai_chat|openai_responses` are explicit canonical spellings;
  the accidentally derived `open_ai_*` forms remain read aliases.
- PAWS05 is accepted because root selects one explicit Mantle Responses or
  Messages dialect and the provider tests enforce their distinct capability
  matrices. Hosted cache/guardrail and Google grounding/code/cache observations
  remain separate rows.
- Profile v3 authority is not converted from its PL08 fingerprint into an
  invented admission generation. Root keeps the code-aware host empty and now
  fails loudly if managed code authority is requested without a matching
  production generation source.

### Verification

Package inventory is now Agent 220, Session 179, MCP 346, TUI 186, Config 75,
CLI 147, AWS 146, Google 142, OpenAI 67, Anthropic 87, Extensions 140 and
extension-host 24 unit/integration tests. Workspace formatting and all-target
warnings-denied clippy pass. The first no-fail-fast workspace run found four
deterministic integration-test defects: one digest missing its `sha256:` kind,
one stale protocol spelling and schema-current/future expectation drift. After
one repair pass, both failed test binaries and the complete workspace rerun are
green at **3,749 unit/integration tests plus 7 doctests** (3,756 executed, 0
failed, 0 ignored).

The final isolated shipping binary reports doctor healthy with four passes and
no warning/failure/skip, the fake headless run ends `[done:stop]`, and raw stdio
app-server protocol 1 completes initialize → session/open → turn/start (`stop`)
→ session/close with protocol-exclusive stdout. No live provider credential was
read or dispatched.
