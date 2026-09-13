# Everything-as-a-plugin architecture

## Architectural objective

The binary is a composition root and protocol launcher. It may know every crate, parse process arguments and choose a profile. It must not own provider logic, setup behavior, tools, commands, UI dialogs, persistence policy or agent behavior.

Every capability is mounted into `Context`, declares every service it consumes, registers contributions as effects and removes them on disposal. Configuration chooses plugins; plugins choose implementations; consumers depend on service contracts rather than concrete providers.

Implemented P02 adds typed service `http`: built-in `http-reqwest` owns opaque HTTP execution, cancellation, bounded status failures and raw SSE framing. Protocol adapters receive framed events and own every byte of JSON meaning. This service sits below LLM/provider code and is replaceable through composition.

Implemented E02 introduces `heycode-exec` at the same low dependency level. Its first service is `subprocess`: a provider-neutral exact-argv contract whose built-in `subprocess-local` provider owns explicit environment construction, bounded output, cancellation and quiescent process-tree termination. Specs require absolute executable/cwd paths, carry the entire child environment and expose no secret-bearing `Debug`. The provider always clears inheritance, maps nonzero/timeout to results, maps infrastructure failures to fixed body-free classes, and creates one processkit private group per spawn. Consuming handle methods make wait/cancel/terminate/kill single-owner; context shutdown and drop settle the tree. Host capability facts explicitly expose the weaker POSIX process-group `setsid` boundary. Shell resolution, sandbox policy and approval remain separate consumers/providers added by E03/E04; the subprocess layer never infers any of them, and existing process consumers are not yet claimed as migrated.

Implemented E03 adds a separate `shell` Service Definition and `shell-local` Provider inside `heycode-exec`. `ShellRequest` contains caller intent and optional overrides; only `ShellBackend::resolve` may materialize the platform shell, cwd, credential-name-scrubbed explicit environment snapshot, timeout and bounded-tail policy into a fully required `ShellSpec`. Execution accepts only that resolved spec and delegates its exact process form to `subprocess`. The model-facing `bash` tool is the first Consumer and `tools` now declares `shell` in `inject`. Invalid timeout values fail at the model boundary instead of falling back. Nonzero and timeout remain results; bounded-tail truncation is visible. `ToolCtx` carries the active cancellation token, so Agent cancellation awaits process-tree settlement before publishing the aborted turn. E04 subsequently moved the sandbox transform below this Consumer into the common subprocess backend.

Implemented E04 moves the sandbox Service Definition and policy vocabulary down into `heycode-exec`; `heycode-sandbox` is only the OS-backend Provider. The service exists even when effective mode is off, where its truthful no-op policy still forms the mandatory process path. `subprocess-local` injects it and applies its exact argv transform immediately before every processkit capture/spawn/interactive launch, so shell, MCP and configured command-credential Consumers cannot bypass policy. Interactive stdio remains provider-neutral through owned input/line handles rather than exposing tokio/processkit types. MCP owns/stops/joins its line driver and drops its private-group process handle; synchronous command credentials use a private runtime only to await the same service. Real tree-survival tests and a source-law allowlist pin the path.

Implemented E01/E05 adds `filesystem-local` and service `filesystem` at the same low layer. Providers own capability-rooted canonical grants, observations with stable identity, metadata, directory creation, bounded reads/search and root-local atomic write/edit behavior. The five model file tools inject and consume this service; traversal/canonical aliases, symlink loops/escapes, root/parent/target/absence races and stale writes fail. Schema v9 inserts the Provider before `tools`.

E06 adds Unix `retained-output-local` below result Consumers. One generation
owns 0700 directories and 0600 single-link SHA-256 objects, but content identity
never substitutes for authority: every lookup also names a host-created logical
owner. Object/entry/generation/read caps are independent. The service constructs
the complete bounded preview envelope itself, so a Consumer cannot cap a body
and then exceed it with id/byte metadata. Shutdown removes only its generation;
non-Unix fails closed pending audited owner security.

E08 composes `lsp-registry` over the same filesystem/subprocess/sandbox world.
Exact stdio definitions are effects; lazy sessions use raw Content-Length
framing, bounded frames/documents/results, workspace-confined file URIs and one
process-tree owner. Caller cancellation sends `$/cancelRequest` and preserves
the session; definition/registry teardown reaps it. Unix default `lsp-tools`
adds safe server listing plus definition/references/diagnostics, spilling large
JSON through E06. The zero-definition default lists empty and starts nothing.
Language-server output carries distinct durable untrusted provenance through
Agent, native runtime and TUI. Trusted project definition discovery remains a
separate configuration owner; X07 owns IDE proof.

Implemented U01/K12 opens `WorkspaceTrustService` before automatic project discovery and publishes it through plugin `trust`. Unknown interactive startup composes a safe ephemeral-session world whose highest-priority modal blocks every send/command; a typed live-service action returns recompose/exit. Headless/ACP require explicit session-only decisions. Composition binds the canonical trust identity to cwd, and project profile/settings/skills/executables consult typed gates. Unix persistence is descriptor-relative, owner-only and cross-process CAS; Windows file persistence remains unsupported rather than using path-based create-then-tighten.

Implemented E10 exposes effective vs available sandbox backend and three exact choice rows. Support is tri-state; unsupported guarantees remain visible but unselectable, and filesystem confinement never implies network isolation. U09/CMD03 consume this report. The E12 native macOS matrix passes. E11 fixes Landlock UAPI/ABI logic and supplies pinned Linux CI plus real bwrap/fallback evidence, but hosted Landlock remains pending. E13 proves only Windows Job Object cleanup and truthful unsupported choices; filesystem confinement needs a spawn-capable Windows Provider.

## Capability completeness

A capability is complete only when all current roles exist:

- Service Definition: stable interface, types, events and errors.
- Service Provider: implementation with explicit configuration and lifecycle.
- Consumer: current product or model-facing path that uses the service.

Do not add empty seams for hypothetical use. Do not put provider-specific fields on a shared service because one Consumer needs them. Provider-specific behavior stays in provider descriptors and resolved operation types.

## Plugin contract v2

The existing Rust `Plugin` trait remains the kernel contract. Ship-ready composition adds metadata and typed contribution registries without turning the kernel into a dependency container framework.

The implemented transition keeps the original plugin name while adding descriptor metadata and typed service keys beside it:

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn descriptor(&self) -> PluginDescriptor;
    fn inject(&self) -> &'static [ServiceKey] { &[] }
    fn apply(&self, ctx: &mut Context) -> Result<(), CoreError>;
}
```

`PluginDescriptor` currently includes stable id, implementation version, source and broad contribution kinds. Every built-in reports `BuiltIn`; compatibility-only test/external implementations default to `Unclassified`. Composition rejects a descriptor id that differs from `name()` and publishes descriptor audit state only after `apply()` succeeds. Scope support, configuration schema id, permissions and exact named contributions land in their dependent tracker slices; `Plugin::apply` remains the single mutation point.

The descriptor patch deliberately did not absorb typed keys or exact contribution ownership. Typed constants landed separately in `K02`; live named attribution follows in `K04`.

`K02` is now implemented: `ServiceKey` is a validated `&'static str` newtype, each service-definition owner exports its `SERVICE_*` constant, and `SEAM_PRE_TOOL` is typed the same way. `Context::{provide,get,has,owner_of}` and `Plugin::inject` accept only `ServiceKey`; string conversion exists only through `as_str()` at diagnostics/config/protocol boundaries. The composition root's `BUILTIN_SERVICE_KEYS` registry pins uniqueness and the constitution-visible names.

## Registration and disposal

All contribution APIs return a disposer. `Context::effect` owns disposers and unwinds LIFO. Composition is transactional: descriptor validation, duplicate-name checks, inject validation and `apply` failures all enter one abort path that calls `Context::shutdown()`. Effects registered by the failing plugin before its error and every prior plugin unwind in reverse registration order; no partial context escapes.

Required contribution registries:

| Contribution | Owner service | Examples |
|---|---|---|
| Provider adapter | `providers` | OpenRouter, DeepSeek, Anthropic |
| Agent runtime | `agent-runtimes` | native heycode, Codex, Claude Code |
| Model catalog | `models` | OpenRouter live catalog, LM Studio local catalog |
| Credential provider | `credentials` | home file, environment, configured command, AWS, ADC |
| Authorization flow | `authorization` | API key, OAuth, device code, cloud login |
| Prompt section | `prompt` | identity, workspace instructions, plan mode |

Implemented K04 exact inventory: `PluginDescriptor` remains the broad permission/shape declaration, while `PluginInventory` records committed rows in exact namespaces (`service`, inference provider, model catalog, catalog/settings/credential provider, authorization flow, tool, command, prompt section, interception seam/layer, UI, external process). Services are captured automatically at `Context::provide`; plugins declare static rows and register discovered rows only during their active apply. Duplicate exact kind/name claims and broad-family mismatches fail composition. The live snapshot backs `/plugins verbose` and is completeness-checked against the actual service/tool/command/prompt registries. Dynamic MCP server processes and discovered tools are included.

U03 implements the `ui` service over an effect-owned `UiRegistry`. Universal metadata is deliberately small: validated slot (`panel|dialog|status`), dotted/kebab id, safe title and signed priority. The registry stores an opaque typed `Arc<T>`; owning plugins define capability contracts without forcing the kernel into a premature rendering DSL, while consumers get typed lookup with wrong-type `None` and poisoned-state failure. Exact `ui_slot` inventory is published before live state, then registration commits and installs a token-checked disposer. Snapshots sort by slot, descending priority and id. Same id may exist in different slots; same slot/id fails. The real TUI injects `ui` and contributes transcript panel, approval dialog and session status, proving the registry is not test-only.

Q03/U19 reuse that single interaction state rather than introducing an
accessibility-specific application. `ScreenReaderSnapshot` is a bounded linear
projection of production `AppState`; it follows the same modal priority and key
router, sanitizes terminal controls and has no content field outside the state
already presented visually. `FlatOutput` publishes only changed frames and
never emits alternate-screen, cursor-addressing, color or animation bytes.
Automatic `TERM=dumb` and explicit `TuiDisplayMode::ScreenReader` choose it.
The CLI owns shipping `--screen-reader` selection and exact-argument
recomposition. A reusable journey harness applies explicit host/terminal/UI
stimuli to the production reducer and records fixed trust/setup/command/MCP/
provider frames without clocks or external services.

CMD01 makes `CommandRegistry` the only discovery source for U04. Each command owns validated id/description, ordered structured arguments (required before optional; variadic final only), active-turn timing, source plugin and optional shortcut. Availability is a separate dynamic projection so unavailable commands remain searchable with a reason. Registry `catalog`, `help_lines`, `names` and `get` include early and effect-time commands in registration order and fail on poisoned state; duplicate ids fail before publication. `/help` renders descriptor synopses. Real composition pins all fourteen current commands and cross-checks each self-declared source against K04 exact inventory ownership. Plugin-owned late commands register as context effects with opaque tokens; shutdown/rollback removes only the exact matching registration.

U04 projects that catalog into a modal owned entirely by heycode-tui. Empty-composer `/` and Ctrl+P open from a fresh registry snapshot; the latter preserves existing composer text. Pure fuzzy ranking checks id before description/source and supports exact, prefix, substring, subsequence and edit distance ≤2 with registration-order ties. Query typing/paste is bounded, Backspace refilters, arrows wrap, Enter inserts `/id` plus a trailing argument space only for available commands and Esc closes. Unavailable rows remain visible with reason; rendered rows include synopsis, description or reason, source, timing and shortcut. Masked secret, onboarding and approval surfaces close/preempt the palette.

U14 joins S14's UI-neutral schema projection to the actual Settings commit
boundary. Plugin `ui` publishes `settings-ui`; custom namespace ownership is an
opaque token registration disposed through Context, while every unclaimed
namespace derives a complete ordered form. TUI `/settings` shares the one
`TuiHandle` panel inbox and becomes available only after both `settings` and
`settings-ui` are attached. Each editable row carries the snapshot revision;
the browser clones the current user section, replaces one dotted leaf and calls
`replace_user` with that expected revision. Only a durable success publishes a
live/restart notice. Conflict reloads authoritative rows. Higher-precedence
project/managed values, secrets, custom delegations and unsupported schema
shapes remain visible and non-editable, so a user-layer write can never be
presented as effective when it is shadowed.

CMD04 navigation is a live human control plane. Panel ids are validated opaque
newtypes carried by `UiEvent`, never strings inserted into the session or
runtime stream. Capability-owned `/plugins` and `/skills` retain command
ownership; TUI-owned `/mcp`, `/agents` and `/hooks` use the one-slot inbox on
the composed `TuiHandle`. The production loop attaches the existing
Settings-backed MCP/plugin operations and the current skills/subagent/hook
services before command execution. Read-only catalog projections are bounded
and data-minimized: no skill body, child authority identity, hook executable,
prompt or arguments cross into the panel. Missing services remain visible as
unavailable/error state rather than being replaced with empty data.

U11 applies CMD01 timing at both composer admission and command dispatch. Active turns are explicit lifecycle state, not inferred from optional spinner verbs. Immediate commands run in a separately joined command task so terminal, approval and agent events remain live; queued and model-scheduling commands retain exact private text in FIFO order and promote only when neither a turn nor command task exists. Interrupting commands render a cancel-default confirmation, restore exact composer text on cancellation, and invoke the current interrupt handle only on explicit confirmation; the command then waits for turn settlement. Execution rechecks dynamic availability/timing. Transcript narration uses descriptor synopses and never command arguments. A04 remains responsible for replacing the native agent's process-lifetime cancellation token with a reusable per-turn owner.

A05/U18 connect the composer to A03 without adding a second queue. Native
active Enter appends Steer, the persisted `queue-follow-up` keymap action (Tab
by default) appends FollowUp and Esc only cancels. AppState stores only a
pending delivery until the event loop calls the Agent; safe narration excludes
text, and transcript/model publication still comes exclusively from Agent's
atomic claim plus durable `user/message`. Full and flat renderers show queue
counts and state-specific keys. One settlement Wake is consumed only after
turn/command settlement and starts one cancellation-owned follow-up before
queued commands; resume seeds the same projection. If UI liveness lag turns a
steer into an idle Wake, the occurrence is durably canceled and requeued as a
follow-up. Delegated runtimes are refused visibly because routing their input
to the native Agent would cross session ownership.

CMD05 adds crate/plugin `heycode-init`, keeping project-instruction persistence out of the binary and generic agent. Its queued `/init` command is a two-phase optimistic transaction. Preview reads at most 1 MiB of regular, non-symlink UTF-8 `AGENTS.md`, detects fixed root manifests, constructs one versioned managed section, and renders a diff capped at 48 KiB plus a 128-bit SHA-256-derived token over observed+proposed bytes. It writes nothing and reveals neither the absolute workspace nor existing project-law text. Apply accepts only the closed token grammar, re-derives the exact proposal twice, rejects stale state, and commits atomically. Existing bytes outside the marker range and existing file mode are preserved; new documents use `0644`. Missing/repeated/reversed markers fail loud. The real command emits only `UiEvent::Info`; production-harness evidence proves preview/apply creates no session/model-visible event.

U05 builds one effective startup snapshot from live handles, not config intent: `Agent::runtime_id()` (`native` until R01), the agent's current provider/model selection, `ApprovalPolicy::kind()`, canonicalized composed cwd and S11 `DoctorRegistry`. The first frame renders health as checking; a separately spawned doctor run owns one cancellation token guarded on loop exit and updates pass/warning/fail/skipped counts or explicit unavailable. The welcome card renders only while transcript content is empty. Provider/model changes update both card and compact status line; permission and health persist after the first message. Future R01/runtime health extends the same sources rather than adding renderer-side detection.

U07 wires `/model` with no args to a typed `ModelPickerRequested` event; an explicit id retains the direct compatibility path. AppState opens in loading state and the loop refreshes the effective provider through `CatalogRegistry::refresh(PreferCache)` without blocking frames. Ctrl+R cancels/aborts the caller wait and starts `Force`; the registry-owned shared refresh may still fill last-good by CAT02 contract. Loop exit owns a parent cancellation drop guard. Default filter is Selectable, and Tab cycles Stable, explicit Tools support and explicit Reasoning support—Unknown never becomes true. Fuzzy ranking covers model id/display/aliases; rows retain lifecycle and tri-state tool/reasoning badges, current id and live/fresh/stale warning provenance. Error/empty states remain actionable. Enter can return only a filtered catalog row and closes; CMD02 persists it and C14 gates a crossing of opaque native state.

U08 changes `/provider` without arguments from a text list to typed `RoutePickerRequested { current_provider, current_runtime }`. TUI snapshots `ProviderRegistry::profiles` and `AgentRuntimeRegistry::descriptors`, retaining complete safe source metadata. Pure projection creates explicit Inference API, Native agent and Delegated agent rows; current rows sort first within their plane, then ids. Fuzzy id/display/detail/class matching and All/Inference/Agent filters never infer loop ownership from a name.

Only activatable rows carry a selection. Inference selection routes through CMD02's durable owner; current native runtime selection is a truthful no-op. Delegated/non-current native rows stay visible with “primary runtime bridge not active” rather than updating status falsely. The status line renders `runtime · provider/model`.

CMD02 adds crate `heycode-routing`. Core plugin `routing` registers Settings namespace/service plus `/provider`, `/model`, capability-gated `/effort`; optional `routing-auth` registers `/connect` and `/logout` only when credential/authorization/onboarding services are selected. This split keeps legacy minimal profiles minimal. Routing schema stores runtime/provider/model together, rejects unregistered routes/unknown fields and currently requires null effort. The startup config selection is its base; schema-v1 `settings.toml` user state overrides it and restores through real composition.

Every interactive or explicit provider/model selection enters `RoutingService`: validate live provider/runtime and catalog evidence, replace the complete user section with expected revision, then apply the returned effective snapshot. A watcher applies external committed generations. `/connect` without target reopens U06; a target uniquely matches a contributed flow and awaits its masked validation/commit. `/logout` maps provider-owned credential reference to one authorization query and invokes authoritative delete; read-only environment/command shadows fail. `/effort` stays visible/unavailable until an active adapter exposes and consumes levels. TUI's delayed `run` dependencies on `runtimes` and `routing` are declared injects. Schema v6 adds lossless v5→v6 routing plus minimal prerequisite migration.
| Tool | `tools` | read, shell, MCP tools, logical server tools |
| Command | `commands` | model picker, setup, doctor, MCP panel |
| UI panel/dialog | `ui` | onboarding, settings, command palette |
| Session projection | `session-projections` | model history, transcript, usage, tasks |
| Compaction strategy | `compactions` | provider-native checkpoint, portable summary, explicit prune |
| Hook | `hooks` | pre-tool, post-tool, pre-compact, session start |
| Settings section | `settings` | provider profile, TUI, permissions |
| MCP server | `mcp` | configured stdio/HTTP server lifecycle |

`compose()` must unwind already-applied plugins when a later plugin fails. A failed composition cannot leave processes, files, listeners or temporary registrations live.

Implemented CAT02 applies this contract to model discovery. The built-in `models` plugin owns `CatalogRegistry`; a provider's `ModelCatalog` source registers as a context effect and disappears on disposal. Per-provider refreshes are single-flight, source-cancelled by lifecycle, TTL-bounded and last-good. A caller cancellation ends only that wait. Failed or invalid refreshes do not publish a new generation; ordinary reads can return an explicitly stale generation with a warning, while forced refreshes fail visibly.

Implemented CAT04 adds a unique effect-owned `CatalogPersistence` provider. Whole provider generations serialize through one commit lane and reach durable storage before their revision becomes visible in memory. Built-in `catalog-cache-file` stores explicit schema-v2 JSON at `$HEYCODE_HOME/cache/models.json` and reads v1 conservatively; version probing, strict mapping, caps, path safety, owner modes and atomic replacement are part of the provider contract. Config/settings retain only the selected provider/model ids—the cache is evidence, not intent.

CAT06 moves advisory provenance onto the value: every non-empty pricing or
performance object carries a validated source and capture instant, while Unknown
has neither. OpenRouter captures one safe `openrouter:models-api` instant after
its full/detail join and applies it to every priced row. Cache schema v2 stores
that provenance; v1 remains readable but restores its source-less advisory
facts as Unknown. A generation timestamp is never substituted.

CAT07 keeps user/project overrides in a separate non-serializing document and
projection. The loader mints per-field source from configured layer/path; the
document cannot claim provenance. Assertions sit beside immutable provider
descriptors, classify narrowing/unevidenced support/contradiction, resolve
precedence per field, report unmatched models and cannot enter the provider
cache. Default composition loads user plus trusted-project layers and the TUI
marks assertions, contradictions and unmatched rows; raw provider filters never
promote a user claim. Machine accessors separately retain origin and accept only
matching/narrowing constraints; unevidenced or contradictory support/limits
fall back to provider evidence. Current product filters still use raw provider
evidence. Any future Consumer that makes a narrowing assertion model-visible
must first persist that exact attribution through C02/C05.

S13 keeps competitor import as a pure, downward-safe `heycode-config` boundary.
The caller supplies already-read text, exact source format and a narrow
user/project/executable authority value; the parser performs no discovery or
I/O. Known safe provider/model/base-URL, credential-free MCP transport and typed
setting rows retain values. Credential/environment/header/OAuth/helper/hook
paths become value-less exclusions, while unknowns retain only path plus
structural kind. Detached candidate construction clones a typed Config, refuses
authority gaps, incomplete enabled MCP rows and name collisions, and clears any
route credential pointer. MCP14 owns discovery, confirmation, source CAS,
unresolved auth references and persistence rather than expanding this parser's
authority.

MCP14 is the next pure boundary. It receives only `CompetitorImportPreview`,
preserves S13 user/project/executable readiness and produces either exact
reconnect-off private definitions or unresolved `McpSecretReference` requests.
An unresolved request retains source path plus environment/header/OAuth/generic
role; names and values S13 erased are never reconstructed. Whole-set extraction
is atomic with respect to enabled incomplete rows, invalid transports/cwd and
existing server ids. Disabled incomplete rows remain preview facts and activate
nothing. Discovery, human binding and persistence remain later owners.

## Service map

The target service map extends the current registry in dependency order.

### Kernel and configuration

| Key | Responsibility |
|---|---|
| `settings` | Versioned, layered, schema-validated user/project settings |
| `credentials` | Secret references and provider-owned credential records |
| `authorization` | Interactive flows that produce credential records |
| `plugins` | Installed/discovered plugin inventory and enablement |
| `profiles` | Ordered plugin composition and migrations |
| `doctor` | Health checks and repair suggestions |

### Model and agent plane

| Key | Responsibility |
|---|---|
| `models` | Provider/model descriptors, live refresh and cache |
| `providers` | Inference adapter registry for the native loop |
| `agent-runtimes` | Native and delegated agent runtime registry |
| `agent` | Live native agent handle and turn control |
| `prompt` | Deterministic prompt/tool assembly |
| `compactions` | Effect-owned native/portable/prune strategy registry and sole durable commit owner |
| `token-meter` | Exact provider count when available, deterministic estimate otherwise |

### Execution world

| Key | Responsibility |
|---|---|
| `filesystem` | Filesystem operations, observation and policy |
| `subprocess` | Process tree lifecycle and environment policy |
| `shell` | Explicit request resolution and shell execution |
| `terminal` | Persistent PTY sessions |
| `sandbox` | Process and filesystem confinement |
| `lsp` | Language-server sessions and navigation |
| `attachments` | Durable binary/image input storage |
| `tools` | Tool registry and guarded execution |

### Orchestration and extension

| Key | Responsibility |
|---|---|
| `commands` | Human-only command registry |
| `mcp` | MCP servers, resources, prompts and auth state |
| `skills` | Layered skill registry and loader |
| `hooks` | Typed lifecycle automation |
| `jobs` | Background operation registry |
| `subagents` | Provider registry and continuation |
| `goals` | Same-session objective and round control |
| `workflows` | Reproducible worker execution |
| `schedules` | Durable follow-up timers |
| `telemetry` | Traces, cost, latency and product health |

### Product surfaces

| Key | Responsibility |
|---|---|
| `ui` | TUI state, slots, dialogs and render contributions |
| `onboarding` | Generic startup/connect wizard state, snapshots, actions and semantic outcomes |
| `session-query` | Search, list, export and traces |
| `acp` | ACP server projection |
| `app-server` | Stable local JSON-RPC v1 turn host plus effect-contributed registry controls used by TUI and future desktop/IDE clients |

## Plugin scopes

Plugins may be declared at these scopes:

- Built-in: compiled with the binary and part of the default profile.
- User: available to every trusted workspace.
- Project: checked into the repository and activated only after trust.
- Local project: ignored by source control and active for one workspace.
- Session: process-local experiment disposed with the session.
- Managed: administrator-enforced, immutable from ordinary UI.

Resolution rules are deterministic. A more specific plugin can override a named contribution only where the owning registry explicitly supports shadowing. Service-key collisions always fail.

K05 implements the scope resolution substrate. Built-in order is the base; unique user, project, local-project, session and managed overlays are applied by fixed precedence regardless of caller order. A layer may enable/disable each id once. Existing enabled rows keep their position while adopting the winning scope; new or re-enabled rows append in layer order. The effective selection consumes factories into `ScopedPlugin`s, and runtime inventory reports scope separately from implementation source. This chooses one instance per id—it does not weaken service or exact-contribution collision rules. K06/K07 supply persisted profile/source metadata; K12 applies project trust.

K06's standalone profile schema is now v2 with backward-readable v1 plugin
rows. Files retain optional name and ordered enable/disable rows; v2 additionally
permits one managed-authority constraint section with an implementation-source
allowlist and denied broad capability families. User/project/local/session
sources cannot declare it. Discovery supplies typed source/scope metadata that
untrusted bytes cannot forge. The inspectable `EffectiveProfileTree` retains the
built-in layer, every overlay/source, every decision (including disabled rows),
winning source/scope, exact enabled order and final managed rules. K11 builds
concrete descriptors and rejects forbidden source/capability before any plugin
`apply`; K07 locates/selects named files and feeds this tree to composition, and
K08 attaches dependency/activation diagnostics.

K07 implements named selection through two real Consumers. `NamedProfileStore` safely lists/loads `$HEYCODE_HOME/profiles/<name>.toml`; plugin `profiles` publishes an effect-owned `NamedProfileService` over that exact store. `--profile` supplies its User-scope layer directly to `compose_world`; queued TUI `/profile` lists the same service, highlights the current row and returns a typed recomposition outcome only after revalidation. The CLI shuts down the old world/runtime, replaces only the existing profile argument and re-enters the authoritative startup load with every other original argument intact. Names cannot traverse, root/files cannot be symlinks, size is capped, and an embedded name must match the selected stem. Legacy complete `[profile] plugins` cannot be combined because its replacement semantics are ambiguous. Schema v21 inserts only `profiles` and `commands` before historical exact TUI rows.

K08 makes `heycode doctor --composition [--json]` explicitly two-phase. `inspect_world` remains the zero-apply API over the production scoped vector: it reads descriptor/inject/provides/exact static declarations, names identity/dependency/service/exact/family failures and never calls a plugin. Only when that graph is healthy does the command run a production-loader activation probe in a disposable canonical workspace. The probe preserves parsed config and profile selection while redirecting sessions/settings/credentials/attachments/catalog state to a temporary root, replacing inference with a fake, disabling watchers/resume and suppressing configured MCP transports. Human/schema-v1 JSON name every plugin's scope and `activated|failed|not_attempted` state plus stable failure stage, omit raw failure bodies and list every suppression. A successful Context shuts down before root deletion. This proves isolated plugin transactions, not credentials, provider traffic, persisted state or MCP health.

Q06 completes the K09/K10 lifecycle lab's terminal ownership. `GenerationRegistry` composes candidates before locking or retiring anything; a failed candidate unwinds and leaves the live generation byte-for-byte reachable, while a successful candidate swaps once. Readers retain `Arc<GenerationContext>` so in-flight work never observes a closed world. `GenerationContext::drop` is the one terminal owner that calls `Context::shutdown()` after the last reader—even if the registry was dropped first. The generated model covers clean/rejected reloads, held/released readers, sweeps, exact seam withdrawal and registry teardown across deterministic sequences. The adjacent six-kind declarative bridge is only PL03 substrate until concrete product registries consume it.

S11 adds the `doctor` service and exact `doctor_check` contribution namespace. `DoctorRegistry` owns async checks as context effects, rejects duplicate ids, snapshots registration order, threads one cancellation token and contains panics/invalid outcomes as fixed codes. Schema-v1 `DoctorReport` is the single human/JSON source: summaries/repairs are compile-time static and runtime evidence is a closed typed enum. Settings reports effective writer availability; credentials reports provider registration count without inspection or resolution; its embedded K08 evidence remains the zero-apply graph. `heycode doctor [--json]` runs those checks in a restricted diagnostic world with no session, settings/credential file creation, catalog/network client or child process. The dedicated `--composition` command additionally runs K08's explicitly isolated activation phase. Provider endpoint/model, sandbox, MCP, delegated-runtime, session and process checks retain their owning tasks.

B09 removes the legacy terminal setup's provider/model table. `ProviderRegistry::profiles()` projects each provider implementation's registry name, safe descriptor, owned default model and optional non-secret credential reference; `SetupCatalog` cross-checks identity and the registered `CatalogRegistry` sources. Model refresh filters effective retirement and sorts ids. Live/fresh/stale provenance is explicit; catalog failure falls back visibly to the provider default, and only that unproven state allows a custom model id. `SetupWorld` composes non-watching settings, credential providers, shared HTTP, models/cache, real DeepSeek catalog and metadata-only DeepSeek/OpenRouter providers—no session/agent/tools/TUI/MCP or dispatch-capable client. Setup config output uses TOML value serialization and symlink-refusing atomic `0600` replacement. The line wizard remains a compatibility surface; U07 supplies searchable model interaction and U10 supplies the integrated authorization-to-ready transition.

B10 carries the selected startup `ConfigMigrationNotice` through `WorldOptions` into a config-owned `doctor-config` check. Current is pass, safely applied home migration is pass with backup evidence, and explicit/project pending migration is warning. `ConfigMigrationEvidence` is a closed schema containing source/backup paths, version classification and typed semantic changes only. `ConfigMigrationPlan`'s original/rendered bytes remain private and cannot enter `DoctorEvidence`. Human and JSON output are projections of the same result; JSON escapes path controls and the human renderer escapes them explicitly. A real unknown-extension canary proves raw config values stay absent from stdout, stderr and report evidence.

## Profiles and bundles

A profile is an ordered list of plugin instances plus configuration references. A bundle is a distributable group of profile rows.

Profile resolution order:

1. Built-in base profile.
2. Selected named profile.
3. User overlay.
4. Trusted project overlay.
5. Local project overlay.
6. CLI/session overrides.
7. Managed constraints validate the effective result.

The effective tree is inspectable with `heycode doctor --composition` and `/plugins verbose`. Every row reports source, config source, dependencies, activation state and failure.

Configuration documents now carry root `schema_version = 26`, and a relative `HEYCODE_HOME` is rejected before discovery. The v0 migration recognizes the exact unversioned eight-plugin snapshot written by the historical home setup wizard, previews newly activated plugin rows, creates `config.toml.unversioned.bak`, removes the frozen profile and atomically commits. B08 changes only the exact generated retired DeepSeek default. Dependency migrations add `agent-options`, UI/runtime/routing/process/filesystem/MCP/model/catalog owners, token counters, app-server/profile services, `compactions` before Agent/subagent, `session-query-jsonl` before TUI, Settings/provider ownership before OpenAI/Anthropic policy Consumers, and only the exact O05/R05/R08 job/runtime bridges required by an existing historical Consumer. V26 admits the optional exact `subagent.worktree_base` and prevents an older reader from silently ignoring isolation intent; older documents add no value. Optional defaults, customized model pins and every intentional plugin choice keep their exact intent. Every plan is idempotent/source-CAS guarded; explicit/project files remain pending.

## Two execution modes

### Native inference mode

The heycode loop owns messages, tools, approvals, session log and compaction. It calls a provider adapter.

```text
user input
  → durable inbox/message
  → prompt + tool assembly
  → request resolution
  → request/header commit
  → provider stream
  → assistant/provider items
  → tool scheduler
  → guarded execution
  → result commit
  → next step or turn end
```

A06 overlaps only explicitly read-only tool calls, serializes admission, and
commits every call/result in model order through one cursor; unknown tools are
barriers. A07 makes request failure closure stage-aware: preparation and stream
failures close their already-started step before `turn/end error`, while
pre-step and post-step invariant failures close only the turn. Bare errors after
`step/start` converge on that owner, and `UiEvent::Error` publishes only after
the durable closure succeeds. C08 remains the read-time crash oracle for a
process that died before any handled closure could commit; it reports Unknown
or Interrupted and never writes synthetic success.

A08 inserts one provider-independent membership decision before request
resolution. `DeferredToolCatalog` unions client schemas with N01 routes; default
`lexical-local` passes catalogs of at most 64 rows unchanged and query-ranks
only oversized catalogs with registry-order ties. One plan filters client
schemas, native routes and prompt tool names together. Cancellation/provider
failure refuses preparation without a full-catalog fallback. `CodeModeSchedule`
can translate selected calls into ordinary legacy or strict stream events but
imports no registry/executor, so every call still crosses A06.

A09 is an effect-owned pre-step layer, not an Agent counter. Default
`loop-budget-settings` registers restart-applied step/token/elapsed/tool ceilings
and an explicit missing-usage policy. Before each step it folds the durable
active turn; restart therefore preserves exhaustion. `turn/end` owns exact
max-steps/max-elapsed/max-tool-calls/unreported-usage/clock-unavailable reasons
alongside max-tokens. Native runtime normalizes them to its smaller `Limit`
settlement. Exact profiles that omit the plugin retain the old eight-pause
compatibility guard.

### Delegated runtime mode

An official or external agent owns its model loop. heycode owns the product surface, lifecycle bridge and local session projection.

```text
user input
  → heycode delegated session event
  → Codex app-server / Claude Agent SDK / OpenCode server
  → normalized runtime events
  → heycode transcript, permissions, status and final result
```

Delegated runtimes must not implement the inference `Provider` trait. They implement `AgentRuntime`, whose contract includes authentication status, model catalog, start/resume/fork, send/steer/cancel, event stream, permission requests, compaction and teardown capabilities.

This makes Codex and Claude subscriptions supported without pretending their tokens are API keys.

Implemented R01 adds crate/plugin `heycode-runtime`. Base plugin `runtimes` publishes an initially empty, deterministic `AgentRuntimeRegistry`; concrete native/delegated plugins later register immutable descriptors as token-owned context effects under exact inventory kind `agent_runtime`. Runtime ids are lowercase kebab-case; provider-native session/turn/request ids are bounded opaque newtypes. Descriptors expose native/delegated loop ownership and tri-state model/resume/fork/steer/follow-up/permission/question/compaction evidence. Account inspection returns only status plus an optional bounded label, never credentials.

The object-safe runtime contract covers cancellable account/model discovery and start/resume/fork. A returned `RuntimeSession` exposes normalized event subscription, send, steer, follow-up, cancel-without-close, correlated permission/question answers, native compaction and quiescent idempotent close; every async operation takes one caller token. R01 owns the typed raw vocabulary. Implemented R02 makes `RuntimeEventNormalizer` the only projection boundary: sequence begins at zero with session-ready, ids/correlations and payload/JSON/control bounds are exact, turns/tools settle, live EOF fails and finite replay EOF succeeds only when quiescent. Runtime usage may repeat per inner model step; errors/Debug contain no body.

R03/R04 implement the first delegated discovery Provider. `runtime-codex` binds
the exact Codex CLI 0.146.0 executable/interpreter/script identity, verifies its
version and initialize user agent, and owns strict body-private JSONL request
correlation plus containment teardown. Current official OpenAI documentation
defines the stable semantics, while locally generated 0.146.0 schema hashes
pin the exact account/model/capability fields because current examples are not
assumed backward-identical.

`account()` opens one fresh connection, calls only
`account/read {refreshToken:false}`, validates the closed API-key/ChatGPT/
Bedrock union and closes. ChatGPT email is validated then discarded; the safe
label is plan-only. Null account plus required OpenAI auth is Disconnected;
when the selected provider does not require it the new exact state is
NotRequired. No heycode code reads auth files or token values.

`models()` reads provider capability booleans, then follows visible
`model/list` pages under a 100-row, 64-page and 4,096-total budget with cursor
and global id/alias uniqueness. Effort lists/defaults, modalities, default/
hidden/upgrade metadata are validated. Reasoning/image and web support map
exactly; namespace-tools true proves generic tool support while false remains
Unknown, and image generation is not confused with input. Explicit upgrades
become Deprecated replacements; unknown lifecycle/limits stay Unknown. Every
account/catalog outcome closes its connection with an uncancelled teardown
token before returning. Runtime registration itself invokes no process.

Implemented A01 adds plugin `runtime-native` after `agent`. It registers immutable exact row `agent_runtime:native`, reports Unknown account state without inspecting credentials, discovers models through the selected provider's existing catalog registry, and binds start/resume to the one composed durable session/workspace. Start refuses a non-empty log; resume requires matching heycode and provider-native ids; fork remains Unsupported. Native send runs the unchanged Agent loop behind a serialization gate, returning canonical decimal turn ids. Cancellation waits for the Agent to append an aborted turn before returning; compact shares the exclusive active-operation gate; close marks closed, cancels and waits for quiescence.

The native event hub listens to the Session's post-commit bus for turn/chunk/final/tool/usage/settlement and only to UiEvent for permission/notices, preventing duplicate or pre-commit projection. A locked snapshot plus 1024-event broadcast ring gives gap-free subscribe and bounded lag failure. Both listeners use `EventBus::on_effect`; context shutdown cancels held native sessions and disposes registry/listeners LIFO. The generic native `RuntimeSession` steer/follow-up methods remain Unsupported; A05 uses the same-session Agent inbox directly, while a later app-server control bridge must own generic/delegated operations. Runtime permission/question paths are also explicit Unsupported.

Implemented A04 replaces Agent-lifetime turn cancellation with `AgentCancellation`: one terminal shutdown token and one identity-owned active child. The async turn gate serializes sends; an active lease exists only for that turn and clears on every return path. Idle cancel is a no-op, while Agent-plugin shutdown permanently cancels the current/future work. Public `send_cancellable` adds a caller token: pre-admission cancellation wins while waiting for the gate and writes nothing; mid-stream cancellation durably appends the partial assistant/step/aborted turn before return.

`runtime-native` creates/stores a child operation token before calling Agent, so cancel cannot be lost between task scheduling and Agent lease creation. TUI similarly publishes a caller token before spawning the turn task, while its persistent interrupt closure cancels both caller and active-Agent handles. Native/direct regression tests abort a hanging first response then complete a fresh second turn; an idle pre-cancel writes zero events/requests and retries successfully.

## Provider request resolution

The native loop builds a `RequestDraft` from durable projections. The selected adapter performs one explicit resolution step:

```text
RequestDraft
  + ProviderDescriptor
  + ModelDescriptor
  + CredentialHandle
  + NativeToolPolicy
  + CompactionPolicy
  → ResolvedCall
```

`ResolvedCall` contains only valid, wire-ready choices: endpoint, protocol, model, auth handle, prompt, messages, provider state, tools, reasoning, output cap, timeouts, retries, native server tools, cache policy and purpose.

Resolution fails before dispatch when a requested reasoning level, input modality, tool type, structured output, state item or native feature is unsupported.

C11 measures that same boundary rather than rebuilding a simplified context.
Strict adapters pass their actual `ResolvedCall`; compatibility providers pass
their actual `ChatRequest`. The resulting `TokenEnvelope` always carries
System, Messages, Tools, ProviderState and Attachments in stable order.
Transcript messages may use the best registered provider-exact counter. System,
tool definitions and lossless state use the explicit local estimator because a
provider endpoint cannot isolate those pieces without inventing a transcript;
native image/PDF bytes remain `Uncounted(Unmeasurable)`. Better-ranked counter
refusals remain attached to the contributor. Agent and native subagents inject
the effect-owned registry, and publish only a complete envelope after the
strict request's durable header/context verification boundary.

U16/CMD07 have a complete neutral product Consumer. Default `status-context` injects
only `commands` and `models`, contributing immediate `/context` and `/usage`
without a new service key or migration. Context joins the Agent's last complete
envelope to its matching durable request header/context and cached pricing,
rendering contributor/refusal evidence, bounds and Unknown cost explicitly.
Strict providers emit one core-validated detailed response before Usage/Finish;
Agent commits it after successful Finish as v2 `assistant/response-metadata`.
Usage projects JSONL turns/routes/completeness plus bounded cache/edit rows.
Cache-aware cost requires a proven uncached/read/write partition and every used
price; ambiguity remains Unknown. `/compact` lists or selects live registry
rows. Provider-owned opt-in Settings are separate POA05/PAN05/PAN06 plugins;
they now resolve before provider publication and default off.

TEL02/TEL03 keep telemetry selection structural. Base `heycode-telemetry` owns
events, redaction, batching and local-off but cannot depend on HTTP or
credentials, so the default provider cannot emit. Separate
`heycode-telemetry-otlp` owns the explicit `telemetry-otlp-http` provider and its
restart-applied, wire-safe settings namespace. Credentials are references
resolved per batch; OTLP/HTTP JSON requests use the composed HTTP service with
bounded body/response/deadline/retry and closed faults. The factory is available
but absent from the built-in order. A profile must disable local-off and enable
OTLP; selecting both fails the shared service key. Production-loader evidence
proves ownership/settings/inventory without contacting a collector.

TEL04 adds a Consumer without changing provider ownership. Optional default
`telemetry-metrics` injects the authoritative session plus the selected
`telemetry` service, seeds bounded lineage/runtime/request-route state from the
existing log without emission, then registers on `Session::bus()` where events
publish only after durable append. Provider request, local/provider-exact/
provider-aggregate tool, portable/native compaction and cache observations use
closed screened dimensions only. Telemetry schema v2 carries one positive
aggregate count; schema v1 reads as one, local counters and OTLP delta sums use
the same value, and zero fails. Four `telemetry_metric` inventory rows and the
listener are Context effects.

TEL05 is an optional default diagnostic edge rather than hidden state inside
the doctor. Plugin `health-history` injects the live `DoctorRegistry` and command
catalog, publishes `HealthHistoryStore` under its own service key and owns
`/health`. The composition root supplies a path under the isolated settings
home. Each command run records a body-free projection of closed check ids,
statuses, codes, durations and evidence kinds; arbitrary diagnostic bodies are
structurally absent. Whole-document atomic replacement enforces owner-only mode,
entry/byte/check/label bounds and preserves recent unhealthy runs without ever
exceeding the hard limits.

Implemented P01/P08 activates this transition explicitly. `RequestDraft` comes from the session-owned route projection; `ResolveSpec` carries exact adapter route/default evidence; `resolve_request` returns a private non-Clone, ownership-consumed `ResolvedCall` containing replay safety/retry policy. Agent refreshes exact catalog evidence, appends request header/context, re-projects and verifies every field, then dispatches with the turn token. DeepSeek and OpenRouter advertise this path after their exact catalog contracts. Provider state buffers until terminal Finish so failed/aborted streams cannot affect later requests.

Implemented N01 adds the `native-tools` service beneath `tools` and `agent`. Candidate registrations are Context effects and retain logical id, implementation id, provider/client/MCP family, optional provider owner and priority. Resolution groups by logical id and chooses matching provider-native, then client, then MCP with stable priority/id ties. The sorted route set enters `RequestDraft` before adapter resolution, survives in `ResolvedCall`, commits in `request/header.options.native_tool_routes` and is included in the C05 exact live/durable comparison. Provider ownership mismatch, duplicate/unsorted logical ids and malformed route identities fail before transport. Schema v14 repairs older exact profiles; N04 separately owns user policy modes.

Implemented N02 splits server-tool truth into two deliberate planes. Lossless provider blocks remain `assistant/provider-item` and are the only model-replay representation. Core `ServerToolCall`, `ServerToolResult`, `ServerToolSource` and `UrlCitation` expose bounded, validated, response-body-free inspection facts; call input/cited text/URLs are redacted from Debug. V2-only `server-tool/call`, `server-tool/result` and `assistant/citation` retain producing request/turn/step/output index. `project_requests` attaches them to the request and enforces insert-once call ids, settle-once results, non-regressing output order and same-route later-request settlement. Agent buffers the entire exact+normalized group until terminal Finish, preprojects the candidate group before the first append, and discards it on stream failure/cancellation. `Pause` starts another durable step only for a strict adapter with validated exact state and stops after eight continuations. Anthropic Messages emits normalized server calls/results/public sources/citations while replaying the complete encrypted blocks unchanged.

PZA04 extends the N02 source rather than adding a provider-specific event. Optional `ServerToolWebMetadata` retains bounded site name, public icon URL, provider result reference and publication string; legacy v2 rows omit it. Z.AI's standalone client validates all seven documented result fields, projects them into `server-tool/result`, and a real append/reopen proves exact durability. Non-default root plugin `native-zai` maps the provider-owned route into N01 with exact inventory and effect disposal. This makes the standalone native search boundary product-reachable without claiming a Z.AI inference route or live credential. PZA05's four MCP specs remain active substrate until credential-aware HTTP/stdio connection owners can create real generations.

PLM04 joins raw service `lmstudio/model-control` to default product plugin
`lmstudio-control`. The latter injects the raw control, native records, shared
catalog, Settings and command registry; owns live `lmstudio-load`; and registers
queued `/lmstudio <load|unload> <target>`. Numeric controls separate omission
from explicit bounded values and hardware booleans are three-state. A load is
pure until the command consumes its plan, then must pass capability admission,
echo verification, exact native instance readback and a forced shared catalog
refresh before success UI. Unload begins from an observed instance id and must
prove its disappearance. Context shutdown cancels the operation token before
withdrawing command/namespace effects. LM Studio publishes global JIT as a UI
Server Setting without a stable mutation API; heycode does not touch private config
and neutralizes surprise loading by never invoking the load endpoint outside the
explicit command. PLM05 keeps Ollama as a separate provider/catalog/inference/
inspector substrate: native `/api/tags` remains protocol Unknown while explicit
OpenAI compatibility uses `/v1`. When configuration explicitly selects Ollama
with a model, conditional `provider-ollama` publishes joined catalog/profile/
inference/inspector services after `models`; the normal `llm` plugin bridges the
concrete no-credential provider into routing. Composition performs no request,
start or pull, and rejects credential references before lookup. Standard
provider/model pickers therefore work in the configured world; a fresh setup
flow cannot guess a default, and the required real installed-model chat smoke
remains open.

PAWS04–06 and PGCP05–07 now cross provider-owned facts into shared adapters and
one baseline product route. `Provider::request_options_for` receives exact
selected-model/N01 context; Bedrock independently validates/serializes cache
points, guardrails and route evidence and normalizes non-overlapping cache
usage. Gemini/Vertex validates selected Search, external-grounding, code and
cache plans, emits normalized events and retains exact replay parts. Claude
Vertex uses a data-driven exact-model Messages dialect with bearer credentials
and the shared thinking/tool/state parser.

Agent invokes the contextual hook after exact selection and before P10/header
commit. Bedrock buffers one complete assistant message; core/session validate,
redact Debug and replay text/tool/opaque reasoning only on the exact route.
Google Developer's maintained default is provider-owned production inference:
`llm` publishes an empty registry/selection first and the later Google effect
registers strict GenerateContent with an operation credential and explicit
empty native policy. AWS factory/settings selection, Google native candidates,
Vertex/Claude factories and hosted canaries remain separate gates; active rows
cannot be promoted from baseline/shared evidence alone.

PDS05 keeps optional DeepSeek capabilities as separate provider-owned routes rather than flags on the production Chat adapter. Strict tools and prefix bind beta Chat; JSON Output binds standard Chat; FIM binds beta `/completions`, non-thinking and 4K. Model-visible tool schemas and prefix text stay in existing durable request inputs, while bounded provider options only activate their wire projection. Current contradictory Flash FIM evidence remains Unknown. PDS04's Anthropic-format route/live proof is independent and remains active.

POA03/PAN03 currently implement provider-owned hosted/server-tool definition and classification layers only. OpenAI names seven exact families; Anthropic names six, with per-model tri-state gates and pause-state retention. Shared Responses/Messages parsers do not yet map those blocks into N02 events, and no production factory selects the provider implementations. Their rows remain active until the whole capability→wire→parser→durable/UI path exists.

POR05 adds the first provider-owned candidate Consumer. Plugin `native-openrouter` registers `openrouter:web_search` after the base registry. Agent resolution removes a client `ToolSpec` only when a provider route wins the same logical id and maps provider web winners to `NativeFeature::Web`; the durable header therefore matches the actual mixed client/server wire set. A data-driven Chat server-tool dialect appends exact definitions, requires an explicit 1..=30 global budget, accepts URL annotations only when configured and disables request replay. OpenRouter's typed heycode policy bounds engine/results/uses/total/characters/calls, while its catalog marks provider-scoped gateway web support because official fallback works for any model. Schema v15 adds the candidate only to exact OpenRouter profiles. Chat citations and aggregate search count are observable; per-call identity is not, so no call event is inferred.

N04 moves selection preference into optional default plugin `native-tool-policy`. It registers Settings namespace `native-tools` with live default plus safe per-logical overrides. Registry resolution classifies only a matching provider row as native; local family is client then MCP, followed by same-family priority/id. `prefer-native` and `prefer-local` may cross families, while `native-only`/`local-only` return a stable admission error if no eligible row exists. Overrides for logical ids absent from the live registry also fail. The Settings watcher applies only committed generations; a sticky application/lock failure makes subsequent resolution unavailable, and Context shutdown removes the watcher before restoring prefer-native. Agent request construction already consumes the resolved route set, so policy changes atomically alter client schemas, native features, durable header and wire on the next request. Intentional exact profiles may omit the optional plugin and retain N01's default without a schema migration.

WEB01 adds crate/plugin/service `heycode-web` / `web`. `WebRegistry` owns effect-registered providers; held services become terminal after shutdown. Request/result types bound query/count/public and final HTTP(S) URLs/body/text/type data and redact query/URL/body from Debug/errors. Provider output is revalidated after the caller-token operation settles. Exact inventory namespace `web_provider` distinguishes implementations from model tools. `heycode-tools` conditionally injects `web`; its model-facing tools contain no HTTP, environment, DNS or parser code and simply map arguments/results/errors.

N03 adds `web-portable`. One provider owns bounded reqwest transport, operation-time Brave key resolution or keyless DuckDuckGo Lite, common result normalization, capped HTML/plain fetch, typed IPv4/IPv6 literal checks and deny-on-any-private DNS resolution. Search and fetch preserve the same logical Consumer interface used when N04 chooses local fallback. Schema v16 inserts `web` plus `web-portable` before older exact profiles whose web-enabled `tools` now inject the service.

WEB02 closes the fetch authority gap. A no-redirect client resolves each domain once, rejects empty/mixed public-private answers and pins the approved address set into the connection; literal metadata/private hosts never reach DNS. Manual relative/absolute redirects repeat scheme/userinfo/host/DNS admission, loop detection and a five-hop cap before sending. Provider search redirects are refused so the Brave credential header cannot cross origin. DNS uses a cancellation-selectable resolver future rather than a detachable blocking-task handle.

WEB04 replaces deployment-order selection with explicit policy. Optional default plugin `web-policy` owns live Settings namespace `web`: independent search/fetch provider ids plus bounded canonical allow/block domain lists. A configured id must exist for that operation at composition; without one, exactly one locally available provider auto-selects, zero is unavailable and multiple are ambiguous. One immutable policy snapshot is attached to the operation. Search results are validated then domain-filtered before publication; fetch checks the initial URL, every provider redirect and the independently validated final URL. The live report preserves registered capability/availability and every selection state. Optional `status-web` explicitly injects `web` and contributes `/web`; it is separate from `status` so intentional exact profiles do not acquire an undeclared optional dependency. No root config schema bump is needed because omission retains unique-auto/allow-all behavior.

ATT01 adds crate/service/plugin `heycode-attachments` / `attachments` / `attachments-local` after the session owner. Core owns validated `AttachmentContentId`, canonical MIME, bounded raster dimensions and redacted `AttachmentMetadata`, keeping future LLM/runtime projections below their common dependency. The store sniffs bytes rather than extensions, validates optional MIME, limits each object to an explicit composition ceiling (32 MiB default; universal 64 MiB maximum) and uses image-rs header-only dimensions capped at 32,768 per side/100 million pixels. Exact bytes hash to `sha256-<hex>`.

The Unix local provider opens an owner-only schema-v1 root, fixed `0600` lock and `0700` object tree. Same-process ownership plus cross-process `flock` serializes publication; a `0600` fsynced temporary hard-links no-clobber at the digest path, unlinks and fsyncs the directory. Existing objects must equal the candidate. Reads recheck type/link/mode/identity/length plus SHA-256/MIME/dimensions. Non-Unix returns `UnsupportedSecurity`; Windows cross-compilation proves the refusal shape only. Admission stores bytes first, rechecks cancellation, then appends v2-only `attachment/added`; session publication therefore never precedes readable content. An append failure can leave only an unreachable immutable object. Active-operation leases allow synchronous session-bus listeners to read reentrantly while shutdown cancels then waits. The admission event alone remains outside model/runtime folds; ATT02 associates native-loop images and X03 routes ACP image/resource blocks through the same admission plus `RuntimeInput`.

WEB03 extends `WebRegistry` with effect-owned exact `web_processor` rows. Portable providers receive a weak processor handle, avoiding a registry/provider Arc cycle; zero matching processors retains the conservative textual fallback, one runs, and multiple fail ambiguous. Optional default `web-extract` injects `web` plus `attachments` and registers `portable-readable`. Raw source and readable output have separate bounds: portable fetch retains at most 4 MiB while the model tool requests 64 KiB output. HTML uses html2text with link rendering. PDF uses lopdf 0.44's bounded load and per-page text APIs, limits each page's decompressed content to 2 MiB, at most 256 pages, emits `[Page N]` markers and caps UTF-8 output. Encrypted, malformed, raw-truncated or bomb-like documents fail before durable source publication.

Extraction owns one `spawn_blocking` handle. Caller/plugin/10-second deadline cancellation is threaded into PDF page work; every terminal branch awaits the handle, so blocking work is never detached. Successful parsing then admits the exact raw bytes through ATT01 with one `AttachmentSourceMetadata` containing final URL, title, retrieval time, raw-truncation state and page count. `attachment/added` is therefore the durable source truth. `WebFetchSource` is constructed from those same typed facts, and `web_fetch` logs an escaped Markdown source link/page count without the content hash. Unsupported binary is never lossy-decoded. The plugin is optional and changes no root config schema.

ATT02 adds optional plugin `agent-attachments` after `agent`. Its injection set
is the architecture: `agent`, `commands` and `attachments`; it effect-binds the
store through a token-checked slot and owns `/attach`. The CLI composition root
only resolves repeatable `--image` paths and calls the same store/Agent APIs.
The TUI owns staged presentation, not durable truth: it keeps up to sixteen
unique metadata records and clears them only after the Agent publishes the
post-commit attachment echo.

Session v2 `user/attachments` is the model-visible association. It references
exact prior admissions, must immediately precede `user/message` and is written
with that message in one buffered append/flush before either bus event. Resume,
fork, compaction, query and route projections validate/preserve that pair.
Neutral projection carries metadata only. Agent rereads immutable content,
constructs bounded `ChatImage` bytes, proves the selected model's image support
through the catalog and refuses legacy/unproven routes before association.
Responses uses `input_image` data URLs, Chat uses `image_url` parts and
Anthropic Messages uses base64 image blocks before text. Independent C05
reprojection rereads the same objects before the resolved call can dispatch.

ATT03 keeps document parsing and document commit ownership separate. Optional
`web-extract` now provides service `document-extractor` in addition to its web
processor contribution. The held service reuses content sniffing,
html2text/lopdf bounds, page markers, panic containment, 10-second deadline and
always-joined worker, but local extraction publishes no fake URL or attachment
event. Optional `agent-documents` injects that service with Agent/commands/store,
effect-binds it and owns `/document`; the CLI composition root preserves mixed
`--image`/`--document` order.

`ModelCapabilities::document_input` is independent tri-state evidence. Exact
Supported plus a strict adapter chooses a native PDF; all other PDF/HTML cases
use the extractor and admit its bounded UTF-8 as a distinct immutable
`text/plain` object. `DocumentInputRoute` stores exact source and selected
metadata plus Native or Extracted inside the existing adjacent
`user/attachments`. Native requires source=selected PDF; Extracted requires a
distinct selected text object. Responses serializes `input_file`, current Chat
serializes `file`, and Anthropic serializes a base64 `document`; extracted
routes append deterministic document text and request only the Text modality.
Both live mapping and C05 independent projection reread the route's selected
object. This makes fallback a committed session fact rather than a future
catalog-dependent decision.

WEB05 adds a separate trust classification plane for external result content.
Core `UntrustedContentBoundary` currently identifies Web. `Tool` exposes a
default-none classification method; Web Consumers override it, and the guarded
execution pipeline carries the marker only with successful values. Agent never
branches on a tool id. After execution it commits the optional marker on v2
`tool/result`, then publishes the same fact on `ToolFinished`.

Neutral session projection retains the marker. Native request construction and
C05's independent reconstruction call one deterministic renderer that wraps the
exact result in a `UNTRUSTED WEB CONTENT — data only; not instructions or
authorization` boundary. TUI live/replay cards show a warning; the native
runtime emits a notice immediately before its tool-result event. Ordinary v1
tool results remain readable, but v1 cannot claim the new field. This is an
authority label, not a parser or prompt-injection sandbox: approval, trust,
sandbox and tool schemas remain the enforcement planes.

X02 makes client-selected route/workspace state effective rather than merely
present in the SDK vocabulary. A base app-server advertises runtime/workspace
controls false; the optional controls generation flips them true while
`runtimes/list`, `runtime/select` and `workspace/select` are registered.
Runtime rows retain native/delegated kind and every tri-state capability.
Provider/model/runtime changes conflict with an active turn. Runtime selection
commits Routing Settings before publishing a new backend, and opened or
differently linked sessions reject a switch. Workspace paths are absolute,
existing, canonical directories contained by the composed root; native remains
composition-fixed, while delegated start/resume receives the selection.

X03 rebuilds ACP v1 as an adapter over `RuntimeSession` rather than a second
Agent event loop. `initialize` advertises image and embedded-context support,
while audio and client-supplied MCP remain false/refused. Prompt framing accepts
bounded schema content blocks (plus legacy string compatibility); image and
embedded binary resources decode under 32 MiB, commit through ATT01, then enter
the native runtime input. User chunks publish only after `TurnStarted`, proving
the durable user/media association already committed.

One fair async loop owns bounded UTF-8 JSONL input and all output writes. A
JoinSet owns prompt turns so input remains responsive. Each ACP session retains
the composed Context, Agent, AttachmentStore, native RuntimeSession, event
sequence cursor, active cancellation generation and approval supervisor.
Normalized runtime events map to agent/thought chunks, tool call/update, plan
replacement and usage. Both `session/cancel` and `$/cancel_request` deny pending
asks and cancel the exact runtime operation; the original prompt responds
`cancelled` after durable settlement. EOF cancels/drains prompts, joins approval,
closes runtime, then unwinds Context. X04 is the distinct stable heycode app-server.

X01 now closes that permission proof. Ask-mode turns convert the real
`InteractiveApproval` notification into one correlated
`session/request_permission`; only the exact pending JSON-RPC id and exact
offered `selected/allow_once` option permits execution. Wrong, duplicate, late,
deny and cancelled responses settle without running the tool, and the next
prompt proves the session is reusable. The approval forwarder is owned by the
ACP session token/task and joins before Context shutdown.

R11 adds the transport-neutral delegated side: strict bounded ACP NDJSON,
validated exact process specs, one caller-supplied process owner per
probe/session, option-derived Unknown-safe model catalogs, exact model
selection and normalized thought/tool/permission/final events. Cancellation
settles pending permission and process close once. `heycode-runtime-opencode`
supplies the production heycode-exec adapter, pins executable hash plus version and
initialized agent identity, clears ambient authority through an explicit child
environment and publishes an effect-owned unavailable row when the optional
installation is absent. Its credential-blind installed catalog canary is not
R10's authenticated GLM turn.

`heycode-runtime-deepseek-harness` is a distinct delegated protocol rather than an
ACP alias. It pins SDK v0.0.1 and the reviewed closed request/notification/event
union, correlates root/child session sequences independently, normalizes the
receipt-to-idle prompt interval and reaps the complete child on cancellation
because the pinned wire has no prompt-cancel method. Its optional installation
also remains visible as Unavailable. Deterministic lifecycle fixtures do not
complete R12 without a runnable local Harness artifact and observed delegation.
A VS-Code-shaped SDK fixture shares the host/session permission loop but is not
X07's real IDE proof.

X04 adds crate/plugin/service `heycode-app-server` / `app-server` / `app-server`.
Protocol v1 has closed JSON-RPC methods `initialize`, `session/open`,
`turn/start`, `turn/cancel`, `session/close` plus contiguous `session/event`
notifications. The typed LocalAppClient still serializes every request and
parses every response; each event is serialized/deserialized before delivery,
so in-process transport cannot bypass the stable wire.

The production backend lazily starts or resumes the composed native
RuntimeSession and spawns no work. The caller owns the turn future, bounded
event channel and cancellation. Runtime replay is filtered by sequence; at
TurnStarted the backend reads the exact committed user text, attachments and
document routes from the session log. Assistant/reasoning/tool/usage/plan,
untrusted Web metadata and settlement become closed app events. TUI's existing
foreground JoinHandle calls LocalAppClient; while it is active, duplicate direct
user/assistant lifecycle UiEvents are filtered, while richer local tool/dialog
and command values remain direct. Cleanup cancels/joins the turn and closes the
client before Context LIFO reaches app-server/runtime/Agent. Schema v17 inserts
the new dependency immediately before historical TUI rows.

X05 leaves that base service intact and adds optional default plugin
`app-server-controls` after `routing-auth`. It injects the existing settings,
credentials, authorization, secret-prompt, provider/model, MCP and routing
services and token-registers one control generation plus fourteen exact
`app_server_method` inventory rows. Disposal removes that exact generation
before the base service, while intentional minimal profiles can omit controls
without losing session turns.

The closed method families are `authorization/{list,start,answer,cancel,logout}`,
`providers/{list,select}`, `models/{list,select}`, `runtimes/list`,
`runtime/select`, `workspace/select`, `mcp/list`, `plugins/list` and
`settings/{list,get,replace}`. Each request/response and masked
`control/event` notification crosses the same serde boundary as turns.
Authorization list is deliberately credential-blind: it never calls a keychain
existence probe and reports `inspected:false`. Start resolves the current route,
requires the flow's non-secret reference to equal that provider profile's owned
reference, correlates only its opaque operation's prompt, and responds only
after authorization-owned durable commit/readback. Dropping the broker future
removes the prompt id so a late answer cannot commit.

Provider/model selection delegates to `RoutingService`; catalog failures retain
both the current id and provider default visibly, but an unproven current id is
not selectable. MCP uses the registry's already-redacted immutable snapshot and
plugin inspection uses the shared live exact inventory. Settings schemas are
wire-dark by default. Only an owner that explicitly calls
`SettingsSchema::with_wire_exposure` publishes schema/layers/resolved values or
permits `settings/replace`; replacement is expected-revision CAS through the
durable writer and synchronous owner watchers before the RPC response. S15 still
owns field/path-level redaction and managed-setting policy for partially exposed
future schemas.

X06 moves every public v1 wire type and the transport-neutral Rust client into
new low-dependency crate `heycode-sdk`. The SDK depends only on core plus
serde/async utilities; it cannot import Agent, runtime, credentials, MCP or the
host. `heycode-app-server` consumes/re-exports those exact types and implements
`AppTransport` for `AppServer`. Its adapter pumps typed internal notifications
through raw JSON strings into `AppClient` without spawning, so TUI and product
tests now exercise the same serialization/correlation code as external clients.

One transport exchange owns a raw request, zero or more raw notifications and
one raw response under the caller token. `AppClient` applies 4-MiB caps, checked
request ids, JSON-RPC/result/error correlation, closed serde decoding, session
identity and contiguous notification sequence per operation. The first sequence
is arbitrary because the host counter is global across requests. A closed
notification receiver disables its select branch; otherwise a biased `None`
would stay ready forever and starve the completed response. The SDK creates no
task. Callers stream through a bounded channel and own/await both a turn future
and any concurrent cancel future.

Session path/new-vs-resume selection remains host composition authority.
`start()` retains the returned current-session identity;
`resume(expected_session_id)` opens that same host-selected session but refuses
a different id. This avoids a client pretending it can select arbitrary host
paths while still giving typed resume safety.

`sdks/typescript` pins TypeScript 7.0.2 and Node >=24.12 (stable built-in type
stripping for the source example) with a lockfile. Its
transport API has the same raw exchange contract and never logs frames. The
client runtime-validates envelopes, every closed event, auth/route/model/plugin/
settings results, safe integers, 4-MiB UTF-8 size, 128-level JSON depth, session
identity and sequence; compile-time interfaces alone are not trusted. Rust and
TypeScript parse `sdks/fixtures/app-server-v1.json`, and both runnable examples
prove start, expected-id resume, typed streaming, concurrent cancel and exact
settlement.

X07 adds a bounded child-process transport without introducing a socket or
cross-user listener. One outer NDJSON envelope correlates each unchanged v1
request/notification/response exchange by safe-integer operation id, so a turn
can coexist with permission/cancel operations. One writer owns stdout;
malformed/duplicate/mismatched input cancels and joins every operation; child
stderr and raw frames are never surfaced.

The composition root exposes `app-server --stdio-v1 --workspace <absolute>
[--resume <uuid>]`, defaults noninteractive workspace authority to restricted,
selects the normal AppServer service and shuts Context down after transport
settlement. The fixture-locked VS Code extension spawns exact argv with no
shell. An installed VSIX/Extension Development Host journey over the shipping
binary proves open, send, overlapping cancel, close, exact-id resume, a healthy
resumed turn and final disconnect. Permission allow/deny is independently
exercised by the same extension transport fixture. No network listener is
claimed; a future remote host would require a separate authenticated endpoint
owner.

MCP12 closes the result bridge without making Agent import MCP. `heycode-mcp`
parses and bounds the protocol's ordered content blocks, annotations/extensions,
`structuredContent`, `outputSchema` evidence and `isError`, then converts them
to `heycode-tools::PendingRichToolResult`. Raw media exists only on that pending
plane. A06's ordered Agent commit cursor admits image/audio/blob bodies through
ATT01, producing prior `attachment/added` records, and then appends the closed
v2-only `tool/rich-result` with immutable references. Core owns the durable
provider-neutral schema so session/TUI/runtime never import MCP. Provider text
is deterministically rendered from that durable object; live/replay UI and
runtime events receive typed metadata plus MCP untrusted provenance. Explicit
JSON null remains Present(null), and unsupported schema assertions remain
NotChecked rather than false conformance.

P02 now enforces the HTTP/SSE split beneath that boundary. `SseDecoder` handles arbitrary network fragmentation, UTF-8 line completion, SSE fields and event bounds; it does not recognize `[DONE]`. Chat Completions consumes `SseEvent` and alone maps provider JSON to text/reasoning/tool/usage/finish chunks. Bounded buffered HTTP returns every status/body to the owning provider plugin for safe classification. A compatible dynamic response publishes validated head metadata before one caller pulls bounded body chunks with cumulative caps, backpressure and the same cancellation owner; it spawns no drain task. MCP uses that path for open-response duplex elicitation, while ordinary providers retain buffered/SSE APIs. Both current branded adapters and PDS01 discovery receive the composed `HttpService`; Responses and later protocols reuse the transport in their own adapters.

P03 adds the first new-contract protocol adapter. `InferenceInput` interleaves neutral messages and lossless provider items; `InferenceEvent` exposes response/item phases, normalized deltas, provider state, usage and finish. Responses output items are preserved byte-semantically as JSON with route/protocol/schema identity, including encrypted reasoning, function `call_id` and `phase`. The parser fails on sequence/identity/settlement drift and EOF before terminal. Session v2 can now retain these items; the adapter remains unmounted until an owning provider profile uses the C05 verified gate.

P04 adds the equivalent Chat Completions adapter. It preserves one assistant choice as a lossless message state item, including `reasoning_content` and multiple index-correlated tool calls. Reasoning request dialect is provider configuration, not shared protocol inference. Current DeepSeek/OpenRouter expose both old and new contracts; the legacy product loop still uses a compatibility projection while provider-profile activation moves to the C05 verified session-v2 path.

PDS01 adds provider-owned DeepSeek discovery as plugin `catalog-deepseek`. The source depends only on the shared model, credential and HTTP services; it resolves credentials per refresh and registers with an effect disposer. A validated live `/models` generation is normalized with current V4 evidence, then merged with dated retirement tombstones for legacy configuration ids. Provider response bytes never cross its stable error boundary.

PDS02 adds reusable `ChatThinkingConfig` route data rather than a shared-adapter provider-name branch. It validates one disabled id and complete enabled-effort wire map. DeepSeek advertises none/high/max with default high: none emits explicit disabled and no scalar effort; high/max emit explicit enabled plus the exact scalar. Enabled mode removes temperature during resolution—before C02 snapshots—and the serializer enforces the same omission. Tool schemas remain, while unproven generic tool-choice/parallel controls are omitted. Unknown custom model descriptors still resolve conservatively without inheriting the V4 default.

PDS03 extends the same route data with a required tool-call reasoning-state policy. On input, an enabled-thinking resolved call rejects neutral assistant tool calls and Chat state whose nonempty `tool_calls` lacks nonempty `reasoning_content`; no transport stream can be constructed. On output, the Chat parser checks the completely accumulated response before creating its lossless state item or successful finish. Failure may have emitted transient start/delta progress, but emits neither `ProviderState` nor `Finish`. Disabled thinking bypasses the requirement. C04 already preserves complete same-route Chat state and suppresses the neutral duplicate, so its projection is the only valid continuation path.

## Capability descriptors

Provider and model descriptors are data supplied by the owning adapter.

```rust
pub struct ModelCapabilities {
    pub input_modalities: BTreeSet<InputModality>,
    pub output_modalities: BTreeSet<OutputModality>,
    pub protocols: BTreeSet<WireProtocol>,
    pub reasoning: Option<ReasoningCapabilities>,
    pub tool_calling: ToolCallingCapabilities,
    pub structured_output: StructuredOutputCapabilities,
    pub server_tools: BTreeSet<ServerToolCapability>,
    pub context_management: BTreeSet<ContextStrategy>,
    pub prompt_cache: Option<PromptCacheCapabilities>,
    pub token_counting: TokenCountingCapability,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
}
```

Unknown is different from false. A catalog that omits a fact cannot be treated as proof that the feature works. The UI displays unknown separately and request resolution remains conservative.

CAT03 now represents model lifecycle independently from capabilities: `Unknown`, `Stable`, `Preview`, `Deprecated` and `Retired`, with optional retirement Unix milliseconds and ordered replacement ids. Resolution uses an explicit comparison instant so a cached deprecation row cannot remain selectable after its own deadline. Provider aliases resolve to a canonical descriptor. Missing and retired configured routes fail with bounded deterministic alternatives; future deprecation returns a visible warning.

## Session format v2

C01 writes envelope v2 and reads/migrates v1 without rewriting it. A resumed historical file is a v1 prefix followed by v2 appends; version regression fails. C02 extends the v2 gate beyond the frozen legacy kinds with request header/context; C03 adds validated lossless provider items. C04/C05 provide protocol-aware projection and exact-adapter comparison. C06 adds insert-once inbox splice accounting; provider profile activation must route through these boundaries before native multi-provider support is advertised.

C02 now adds `request/header` and `request/context` to the v2-only gate. Headers retain canonical route/protocol/target, secret-free auth binding, exact rendered system text plus verified SHA-256, full tool schemas and explicit effective options/purpose. Context records known capacity/output and the exact catalog revision/timestamp pair. Both use `RequestId`. They are durable but not yet emitted by the legacy product loop; C04/C05 own their implemented reconstruction and independent pre-dispatch comparison.

C03 adds v2-only `assistant/provider-item`, correlated by request/turn/step and output index. The boxed core item retains provider, canonical model, protocol, state kind, schema version and lossless object JSON. Protocol-kind validation prevents Responses output items and Chat assistant messages from being relabeled across adapters. Legacy history ignores these events; C04 consumes them in chronological provider input.

C04 now reconstructs `ProjectedRequest` for every header. It validates unique context and provider-item settlement, applies compaction to every input plane, substitutes complete same-route state for duplicate neutral assistant output, preserves reasoning-only partial state alongside fallback, and excludes incompatible opaque state on route change. This is a pure log projection; C05 compares it with the live call at dispatch.

C05 now implements that comparison. The live `ResolvedCall` first maps to validated C02 snapshots for commit. Verification then receives the independently projected request and compares every route, target/auth class, prompt text/hash, tool, effective option/default, context/catalog fact and ordered input. Success returns an ownership-consumed wrapper borrowing the exact adapter instance. No mismatch can construct a transport stream.

Q04 makes that invariant reusable for fixtures without moving Session into the
LLM crate. `heycode_agent::testing::verify_persisted_replay` appends the exact C02
header/context to a caller-seeded Session, flushes, independently reopens the
physical JSONL, selects one request id through `project_requests` and calls C05
with the exact adapter. Success returns `VerifiedResolvedCall`, not a parallel
boolean. Chat, Responses, Messages, Gemini and Bedrock share the matrix. A test
overwrites a persisted envelope byte while leaving the Session's in-memory
events valid; only a real reopen observes it, and the failure class contains no
request content.

C06 implements v2-only `agent/inbox/splice`. Follow-up, steer and inject messages use one global insert-once id ledger; pending/claimed/cancelled/replaced state survives resume and remains independent from compaction. Operational inbox text is excluded from provider projections until admission is separately durable as `user/message`. Tool-result error state also remains neutral/durable through request reconstruction so adapters such as Anthropic can serialize it exactly.

C09 adds one owner-only SQLite summary index as a disposable projection. Every
compare/rebuild begins by reopening the authoritative active/archived root and
shared-prefix JSONL set through the ordinary bounded query reader and binds safe
summaries to exact logical-line hashes. Missing/stale/corrupt/interrupted or
incompatible SQLite is a rebuild status; invalid JSONL is a truth error. Rebuild
uses the lineage lock, a private staging database, a second source projection
and atomic publication, so no crash or index row can modify or replace session
truth.

O02 composes one native subagent Provider over the O01 registry. Fresh creates a
new metadata-bound durable session and receives no parent history. ForkParent
uses C07's exact persisted-prefix hash lineage and stores only the child suffix.
Continuable retains the same child Agent/session; OneShot returns no handle.
Provider capability evidence for fork/continuation/interrupt is tri-state and
selection never downgrades an unproven request.

O03 separates registry lifetime from caller authority. Every registry owns one
private token and mints the root authority for the host session. A derived
child authority retains that token while changing owner to the child session
id, incrementing depth and recording whether its own lifetime may retain
children. Retained handles store owner; list/send/interrupt/close require an
authority from the same registry and exact owner, making foreign and unknown
ids indistinguishable. One-shot children cannot create continuable descendants;
continuable follow-ups scope the retained authority so depth cannot reset.
Unbound requests fail before provider selection. Provider output is admitted
before insertion: continuation/handle presence, handle id and duplicate live id
must agree; rejected handles are closed. Context disposal still interrupts all
children regardless of owner because it is the terminal lifecycle authority.

C12 publishes effect-owned service `compactions` with three default rows:
`provider-native`, `portable-summary` and `prune-oldest`. A strategy receives a
read-only durable snapshot and returns a `CompactionPlan`; it never owns session
mutation. The registry brackets preparation, rejects strategy/concurrent writes,
validates the exact prefix and descriptor/replacement class, then appends one
settlement. `/compact`, the pressure middleware and native RuntimeSession all
select through this service. Portable summaries can use legacy or strict
inference with `CallPurpose::Compaction`. Optional lower-layer
`InferenceAdapter::native_compaction` returns a bounded exact-route checkpoint,
which the Agent commits as v2-only `compaction/native`. Route projection shadows
the prefix only for the same provider/model/protocol; neutral or incompatible
routes retain original history. A settlement cannot name itself or future seqs.

C14 layers an explicit policy over that safe projection. The session owner
finds the winning compaction settlement using later-wins ties; a mismatched
native winner yields its route and `EventCount(settlement_seq)` pre-checkpoint
fork boundary. Routing refuses provider/model persistence without a choice.
`portable` uses the current provider, verifies the later summary cleared the
barrier and then commits Settings/live route; `fork` creates a shared-prefix
child and leaves the current world unchanged; `cancel` writes nothing. Events
after a turn end do not reopen it—fold boundaries match start/end by turn id.
App-server v1, which has no resolution field, returns Conflict.

C15 exercises the whole projection over one reopened 1,000-turn/9,011-event
file with ten alternating portable/native checkpoints. It asserts sequence,
all request correlations, retained exact-route state and incompatible neutral
fallback without a wall-clock claim.

U15/CMD06 turn the C07 query boundary into a complete human lifecycle without
moving authority into TUI code. `SessionQueryService` owns bounded filtered
pages plus create/resume/fork/rename/archive/delete/restore/export. Archive is
an in-place zero-byte marker so shared-prefix paths never change. Unix live
logs keep shared locks; one owner-only root lineage lock serializes every fork
with delete/restore/export across processes, and delete moves only a closed
leaf into recoverable trash after the descendant/current proofs. Lossless
export copies each ancestor's exact physical suffix only if it still matches
the hashes validated at open. TUI contributes the panel and seven queued
commands as effects, then returns a typed committed id; CLI replaces prior
resume selectors and recomposes through the authoritative loader. Schema v23
inserts `session-query-jsonl` before historical exact TUI Consumers.

C10 adds the third export form without trusting content screening as proof.
`RedactedSupport` emits only static event kind names, seq/time, closed outcome
ids, counts/booleans and numeric usage/cache/edit facts. Arbitrary prompt,
answer, reasoning, tool/provider, URL, title, path, attachment-name and opaque-id
values have no serialized field. The trace is capped at 10,000 newest events
and 8 MiB with an explicit omitted-prefix count, commits owner-only beside the
other exports and is selected as `/export support`. Q18 still owns the larger
preview/runbook bundle that joins config/plugin/health artifacts.

POA04/PAN04 close the composition edge. `openai` and `anthropic` now pass the
root inference-provider admission list; the credential-backed `llm` factory
constructs their strict provider over the composed HTTP service and current
model. Their adapters expose C12 native compaction, while `/compact` remains
portable by default. Real-composition tests disable the sanctioned fake, seed
one unique owner-only temporary credential reference, inspect the strict/native
interfaces and issue no request. OpenAI's model-gated prompt-cache option now
projects every declared member through ordinary Responses calls. OpenAI and
Anthropic detailed facts normalize through the same v2 durable/UI plane.
Provider-owned restart Settings default off; root resolves OpenAI cache policy
before publication and applies one Anthropic cache/edit generation to both
inference and token counting. Anthropic token counting is an
`Estimated(ProviderTokenizer)` evidence rung above the local byte ratio, not an
Exact count.

New durable facts include:

| Event | Purpose |
|---|---|
| `workspace/trust` | Effective trust decision and root identity |
| `agent/runtime` | Native or delegated runtime descriptor |
| `request/header` | Provider, model, endpoint identity, rendered prompt hash/text, complete tool schemas, explicit options and purpose |
| `request/context` | Context window, output cap and capability revision |
| `assistant/provider-item` | Provider-owned continuation item such as reasoning state, thought signature or opaque compaction item |
| `assistant/response-metadata` | Neutral exact cache read/write/partition/reasoning and applied context-edit/cache-impact facts |
| `server-tool/call` | Logical and provider-native server tool identity |
| `server-tool/result` | Correlated outcome/count/safe error code and public result sources; no raw body |
| `assistant/citation` | Bounded public URL/title/excerpt/range attached to assistant output |
| `compaction/applied` | Portable summary or explicit prune marker plus replaced prefix boundary |
| `compaction/native` | V2-only exact provider/model/protocol checkpoint, strategy, boundary and optional normalized usage |
| `model/selection` | Human-selected route and reasoning choice |
| `permission/preset` | Effective sandbox/approval selection |
| `attachment/added` | Implemented ATT01 content address, canonical MIME, exact bytes, safe name and optional validated raster dimensions; never inline content |
| `user/attachments` | ATT02/ATT03 exact prior attachment records associated only with the immediately following user message, plus optional immutable Native/Extracted document source→selected routes; bytes remain content-addressed |
| `agent/inbox/splice` | Queued follow-up, steering and injected context lifecycle |

Opaque provider items are validated and tagged with provider, protocol, model and schema revision. They are never parsed by other adapters. C12 safely retains the original prefix on an incompatible route. C14 derives the winning native barrier, refuses direct Settings mutation and implements portable recompact, pre-checkpoint fork or cancel rather than silently carrying opaque state across providers.

Every event kind has an exhaustive enum arm, stable wire tag, v2 known-kind registry entry, fixture and projection decision. The v1 known-kind gate remains frozen. v1 payloads migrate through the current enum while retaining source `event.v`; on-disk rewrite is an explicit export operation, not a silent mutation.

## Model-visible request invariant

Immediately before dispatch, the loop independently reconstructs the request from the committed log and compares canonical prompt, messages, tools, route and provider-state items with `ResolvedCall`. A mismatch fails the turn before network I/O.

Dynamic system context such as cwd, time, tool catalog, skills and plan state becomes a durable request header or source event. Live registries may propose the next value, but the committed value is what dispatch uses.

## Events and interception

The target event domains are:

- Session events: durable, append-only facts.
- Agent events: live turn, request, inbox and status coordination.
- Tool events: pre-execute, execute, post-execute and result.
- Provider events: catalog refresh, auth, dispatch, retry, rate limit and stream.
- Product events: UI/command/settings/plugin/MCP state.

Waterfalls are around-middleware and must delegate unless they deliberately own the replacement or denial. Notifications are contained. Serial lifecycle checkpoints are awaited and have explicit failure semantics.

Required waterfalls:

- `agent/pre_step`
- `agent/request`
- `agent/request_error`
- `tools/pre_execute`
- `tools/execute`
- `tools/post_execute`
- `provider/request`
- `provider/response`
- `compaction/select`

Do not add a seam without a current listener and caller.

P10 now implements the two provider rows as one global
`provider-interception` service owned by every `llm` implementation. Request
layers receive only durable-header mutators and run before adapter validation,
request append/C05 comparison and transport. Response layers receive exact
route context plus one normalized event or body-free failure class and run
before telemetry/accumulator/session/UI. Both registrations are Context effects.
Strict adapters publish a secret-free auth preview that final resolution must
match; Agent's native-tool Consumer post-checks downstream route edits and
rechecks immediately before resolve; optional `provider-telemetry` records
closed failure dimensions through either local-off or OTLP. Checked waterfall
completion makes an untyped missing-`next` short-circuit fail closed; typed
policy codes and layer failures are body-free. Agent cancellation settles a
parked layer, and response refusal cancels the adapter operation. Compatibility
Chat and the separate native-compaction result type remain explicitly outside
this exact seam. Profile scope resolution is followed by stable concrete
service-dependency ordering, so an opt-in provider appended by a higher layer
still precedes its Consumer (GOTCHAS #238/#239).

N05 layers an optional `request-transforms` service over that request seam.
Provider policies register complete immutable descriptor generations as
effects; the registry exposes requested/effective/effect/cost independently and
sorts exact ids. Its post-`next` layer inserts a missing provider option,
accepts exact equality and refuses conflict without overwrite. OpenRouter's
provider plugin contributes context compression, file parsing and response
healing; current account enforcement keeps effective state Unknown, disabled
rows carry no cost and enabled rows distinguish Unknown, documented-free,
upstream-token and published page fees. The generic registry contains no
OpenRouter ids or wire fields (GOTCHAS #240).

N06 preserves the two server-tool evidence forms rather than merging them.
Exact `server-tool/call|result` ids supply outcomes; v2
`server-tool/usage` carries a positive provider aggregate count, evidence class
and Unknown/published cost. Chat emits OpenRouter `web_search_requests` without
synthetic calls, Agent buffers it to Finish, and request projection enforces
one logical aggregate per request. TEL01 groups local, provider-exact and
provider-aggregate rows separately; `/usage` exposes request/success/error/
unsettled/cost facts and no arguments, results or queries (GOTCHAS #241).

## Settings and credentials

`S01` now mounts an immutable layered `settings` service from a dedicated `heycode-settings` plugin. Namespace owners register JSON-schema metadata, authoritative validators, schema defaults, an optional composition base, and `live|restart` timing. Provider documents contribute detached user and trusted-project sections. Plain objects merge recursively in the fixed order `defaults < base < user < project`; arrays and primitives replace. Invalid namespaces/layers/defaults/resolved values and duplicate registrations fail before publication. Snapshots expose read-only references, and every registration installs its unregister operation as a `Context` effect so composition rollback and shutdown remove it automatically.

`S02` adds a synchronous `SettingsWriter` boundary and `replace_user`: resolve and validate a candidate, persist it, then publish a new immutable snapshot only after success. The `heycode-settings-file` provider owns a standalone schema-v1 TOML document (`$HEYCODE_HOME/settings.toml` in the product), re-reads before each serialized write, edits with `toml_edit`, preserves unrelated comments/root tables/namespaces, and commits through a same-directory atomic replacement. On Unix the replacement itself disables inherited mode preservation and creates at `0600`; there is no fallible chmod after commit. Existing symlinks/non-regular files and future schema versions fail before service publication. An optional already-trusted project file is loaded read-only and remains higher precedence; the CLI does not supply it until the `U01/K12` trust gate exists.

`S03` adds a monotonic raw-user-section revision to every snapshot. `replace_user(..., expected_revision)` checks CAS before persistence; stale callers receive expected/actual conflict data and cannot touch disk or memory. All commits share one serialized operation lane. Namespace watchers are context effects, observe committed snapshots in order, and run after publication; panics are contained. Because callbacks execute synchronously inside the serialized lane, recursive write/publication attempts fail immediately with `ReentrantWrite` instead of deadlocking—callbacks may read or schedule later work.

Providers publish complete detached document generations through `publish_documents`. Every registered namespace resolves and validates before any commit; a bad external edit keeps the entire last-good generation and notifies nobody. A changed raw user section advances its revision; project-only resolved changes retain the user revision. The file provider uses `notify` 8.2, canonicalizes watched targets once (including macOS `/var`→`/private/var` identity), filters access noise, and bounds burst draining at 250 ms so a noisy directory cannot starve reload. The watcher/reload worker is a plugin effect. `WorldOptions.settings_watch` is explicit: true in the product, false in isolated composition tests.

S03 still exposes wholesale namespace replacement only. X05 adds an explicit whole-namespace `with_wire_exposure` attestation and keeps every unattested schema/value dark at the app-server boundary. Path-safe partial mutation, secret field roles, verified redaction and managed-setting policy remain `S15`; an incompletely safe namespace must stay unattested. A provider setting holds a credential reference such as `OPENROUTER_API_KEY`, never its value.

Credential provider precedence:

1. Explicit process environment for this launch, read-only.
2. Trusted command helper, read-only and operation-time resolved.
3. heycode home credential file, writable and the only built-in persistent store.

Provider-defined ambient cloud/runtime identities remain separate read-only inputs;
heycode never stores their credentials in an OS keychain.

`S04` now defines this boundary in `heycode-credentials`. `CredentialReference`, `CredentialKind` and `CredentialProviderId` are validated opaque ids. Providers separately `inspect` safe configured/source/writable/validation metadata and `resolve` a `CredentialSecret`; inspection must never resolve. `CredentialSecret` wraps `secrecy::SecretString`, is neither serializable nor displayable, redacts `Debug`, zeroizes on drop and exposes only through an explicit operation-boundary method. Serializable `CredentialDescriptor` has no value field by construction.

QSEC02 closes the provider-error escape hatch. Provider trait failures are
free-form boundary data even though the contract asks implementations to redact
them; `CredentialsService` therefore discards the body and retains only the
validated provider id. `resolve_route` adds only the requested non-secret
reference. The unified canary starts in a deterministic environment provider,
is positively observed in the strict adapter Authorization header and is then
proven absent from provider body/system prompt, UI/debug, request projection,
physical JSONL, process diagnostics and structural support export. The outbound
auth boundary is the only allowed observer.

Providers register as context effects and resolve by `(precedence, id)`. The first configured provider is authoritative; if it is read-only it shadows writable fallbacks rather than making a write look possible. Duplicate ids and inspect/resolve contradictions fail loud. The default product mounts environment, an empty command provider and the home file provider; all registrations are effect-owned.

`S05` now mounts `heycode-credentials-env` at precedence 0. It reads `CredentialReference` names from `std::env::var_os` without mutating process state, treats missing/blank values as unconfigured, reports `Environment + writable=false`, and converts to Unicode only during explicit resolution. The registry's write path stops at the first configured provider: a configured environment value returns `ShadowedReadOnly` and never falls through to the file store. A deterministic map-backed reader gives tests the same contract without edition-2024 unsafe environment mutation.

`S06`'s native keychain implementation was retired on 2026-09-05. The shipping graph has no keyring dependency or keychain factory. Bootstrap, setup, authorization and runtime use the owner-only home credential file; doctor never opens an OS store. No read, write, presence, migration or cleanup operation accesses old OS entries. Root schema v29 replaces legacy complete-profile selections with `credentials-file`; named/current selections report the retired row with repair guidance.

`S07` mounts `heycode-credentials-file` at precedence 20. Its root is explicitly supplied per world, rejects symlinks/wrong file types, and is tightened to `0700` on Unix; `credentials.toml` uses schema v1 under `[credentials]` and every atomic replacement is `0600`. Reads validate the file at plugin load and on each operation so rotation is immediate. Parsed raw strings and serialized output are zeroized; secret-map values use a drop guard because `zeroize::Zeroizing` does not implement `BTreeMap` wiping.

`S10` mounts an empty `heycode-credentials-command` provider at precedence 5, between environment and the file store. A trusted consumer constructs immutable reference→command specs: exact executable + argv, no implicit shell, zero/over-60-second deadlines rejected, at most 128 bounded components, empty child environment except an explicit validated name allowlist and stdout capped at 64 KiB. Output must be one non-empty UTF-8 line after terminal CR/LF removal. Each resolve starts again so external rotation reaches the next operation. E04 binds configured providers to the composed subprocess service, so timeout and sandbox policy cover the whole helper tree; empty, nonzero, oversized, invalid-output, spawn/read/runtime/worker failures remain fixed messages containing no argv/stdout/stderr/OS text. The synchronous credential trait still uses a private current-thread runtime only to await that async service. The built-in instance has no commands: project/user spec activation remains deliberately unavailable until K12 trust.

Legacy `credentials` (`REFERENCE=value`) migration is recoverable and idempotent: parse/validate old bytes, merge only missing/equal references into the new store, atomically commit/validate new, atomically create or verify byte-exact `credentials.legacy.bak`, then remove the old active file. A conflicting value or backup stops before deleting legacy. The real installation migration tightened `/Users/naresh/.heycode` from `0755` to `0700`, wrote new/backup files at `0600`, and removed only the active legacy file.

Real LLM construction now happens inside the `llm` plugin after credential
provider plugins apply. Startup preflight resolves the configured reference for
presence/validation and drops that value; the constructed DeepSeek, OpenRouter,
OpenAI or Anthropic adapter retains only a registry-backed `RouteCredential`.
Each operation resolves its exact query once, retries share that operation's
value and the next operation sees rotation. A resolver-route mismatch fails
before registry access and an absent route never probes a different reference.
Strict request snapshots therefore record the configured non-secret handle,
not `AdapterOwned`. Startup/setup compatibility uses the same service/provider
implementations (not the old parser) for preflight and writes. New references
use the home credential file; existing authoritative file records remain file
records; environment shadowing fails. The legacy parser remains only as a
directly tested migration parser until its public compatibility surface can be
removed.

The credentials plugin also owns a `credentials` settings namespace containing only `references: { owner: reference }`. Its validator rejects malformed persisted references during composition. Runtime configured/source/writable state comes from the credential registry's safe descriptors, not from stored settings, and secret values never enter a settings snapshot.

Credentials resolve once per operation and are never copied into child environments unless the child plugin explicitly declares that reference. Logs use source and last-validation time only.

Authorization flows own the human interaction and credential commit. Examples include masked API key entry, Codex browser/device-code login, Claude native login status, MCP OAuth, AWS SSO and Google ADC setup.

MCP05's OAuth boundary now resolves protected-resource and authorization-server
metadata, then selects pre-registered client information, CIMD or bounded DCR
in that priority. Resource, issuer, client, redirect and S256 state remain one
binding through callback, token exchange, refresh and credential records;
substitution fails before a code/token/client secret is sent. Q05 drives stdio,
Streamable HTTP and OAuth through shared bounded success/cancel/hostile
assertions. MCP15 adds official Inspector 2.4.0 against real local stdio/HTTP
fixtures and a separate heycode-driven stateful local OAuth authorization/resource
server. The latter retains semantic HTTPS identities over a plaintext loopback
test transport; Inspector did not drive browser OAuth. MCP11 adds the
pull-owned dynamic HTTP response and proves a stream held open pending its
concurrent elicitation reply. Root now mints one session id before apply, uses
it for create-new session storage and one exact router per configured server,
shares the ordinary Agent approval policy, and mounts TUI progress/log/form/URL
elicitation plus durable lifecycle hooks. MCP11/MCP13 are complete; O09 retains
only concrete structured handler-provider composition.

Settings-derived provider policy resolves during activation from the one
registered `SettingsService`, never from a second preloaded file snapshot.
Fixed provider/catalog inventory is declared before apply; configuration-
dependent native rows are contributed inside the same transaction. Schema 28
repairs only exact profiles that already select the affected OpenAI/Anthropic
or cloud inference Consumer. A managed code-authority fingerprint remains a
reference to PL08 evidence, not a generation root can recreate.

`S08` now mounts `heycode-authorization`. Flows register unique validated ids as context effects and publish safe label/method/interactive descriptors. An invocation owns one `CancellationToken`; cancellation is checked before flow execution and again after the secret grant, before persistence. A flow returns only `AuthorizationGrant(CredentialSecret)`—it cannot return or claim success. The registry commits through `CredentialsService`, reads back the authoritative safe descriptor, verifies the accepting provider remains configured/authoritative, and only then returns a secret-free `AuthorizationReceipt`. Duplicate/unknown flows, redacted flow errors, credential failures and precedence races are typed.

`S09` adds `heycode-authorization-api-key`. `SecretPromptRequest` always marks API-key input masked; U06 supplies the TUI implementation, while the currently mounted deferred prompt fails safe instead of reading stdin. Provider-specific validators run before grants: OpenRouter uses official `GET https://openrouter.ai/api/v1/key` and optional `/models`; DeepSeek uses authenticated `GET https://api.deepseek.com/models`. Stable classes are `unauthorized` (401/403), `host` (bad URL/endpoint/protocol), `model` (authenticated but configured model absent), `network` (transport/timeout/429/5xx), and `cancelled`. No response body enters errors. A validated grant records safe checked-at evidence in the committed credential descriptor; failed validation writes nothing.

`U06` replaces the deferred prompt with `InteractiveSecretPrompt`, a service-backed oneshot broker. Notifications carry id/prompt/reference/kind/masked only. The TUI's raw String is private, capped at 8192 characters, never put on EventBus/transcript/session/debug, and zeroized on drop; submission moves it directly into `CredentialSecret`. Runtime-class method pages are derived from `AuthorizationDescriptor` method/query metadata, and selected flows run asynchronously through S08. Invalid keys return the stable notice to the method page. Successful receipts land on a visible Connected/Continue page.

`U10` closes the first-run transaction. API/router rows are limited to the effective provider's contributed flow, whose query retains the selected custom credential reference. A receipt exists only after registry-owned write, validation record and authoritative readback; only then does Continue return `RecomposeConnection`. The CLI shuts down the old Context, drops its Tokio runtime and repeats the exact original argument vector. Startup presence, preflight validation, flow construction and LLM composition all resolve the configured reference first and use the provider-owned default only when absent.

The first ready composer does not force redundant model and permission pages. The effective provider's registered profile owns its default model, and `SandboxCapabilityReport` proves the current permission row selectable. Users retain `/provider`, `/model` and `/permissions` for explicit changes. A production-loader test uses the ordinary built-in profile, commits through the real file provider in an isolated root, tears down and recomposes a real credential-backed provider world with inactive onboarding and matching registry default.

The TUI event loop is itself an operation owner. Its fallible body runs inside a nested async result boundary so `?` cannot bypass cleanup. Every terminal exit/error cancels authorization, admitted turn, doctor and catalog-wait tokens before joining their handles; commands are either Agent-cancelled for durable model settlement or aborted and joined for human-plane work. The original loop result is returned only after quiescence.

`S12` adds bounded validation records inside `CredentialsService`. Each record stores safe validation state, expiry, and an internal SHA-256 fingerprint of the resolved secret; the fingerprint is never serialized or exposed. Descriptors project `Stale` after TTL. Resolve compares fingerprints and clears validation immediately when a provider's secret rotated; write/delete/provider disposal also clear affected records. Authorization seeds a 15-minute valid record only after committed write/readback. Binary preflight validates existing configured credentials before a normal composer/turn and seeds the composed cache; interactive failures enter repair onboarding, while headless fails before model dispatch.

POR01 adds `heycode-provider-openrouter` and plugin `provider-openrouter`. It owns the exact `openrouter-api-key` authorization row, preserves a selected custom `CredentialQuery`, constructs the official key/model validation plan and removes the row on Context shutdown. The shared `authorization-api-key` plugin retains DeepSeek only. Exact inventory, production composition and schema-v12 migration prove the ownership change. `OpenRouterProvider` remains reusable protocol/client code in `heycode-llm`; its provider-owned default is `z-ai/glm-5.3-flash`. POR02–POR05 subsequently add catalog, strict routing/reasoning and native web surfaces.

POR02 adds sibling plugin `catalog-openrouter`. It fetches the complete unpaginated public `/models` generation, validates count/identity/limits/modalities/parameters/lifecycle before continuing, then cross-checks the default through singular `/model/z-ai/glm-5.3-flash`. Full and detail selected fields must agree before the immutable generation publishes. Current top-provider limits are used conservatively; request ids remain strict while display-only whitespace is normalized. Empty supported-parameter lists are valid explicit evidence. The source is an effect and survives no Context shutdown. POR03/POR04 then activate strict route/reasoning policy; POR05 layers provider-scoped gateway web evidence.

POR03 adds generic `ProviderRequestOption` to core. It is provider/kind/schema tagged, JSON-object-only, recursively bounded and redacts its data from Debug. `RequestDraft` and private `ResolvedCall` carry exact options; `request/header.options.provider_options` persists them with an empty-list default for older v2 logs; C05 compares the exact object before dispatch. OpenRouter's typed policy validates provider order and materializes official `allow_fallbacks`, `require_parameters`, `data_collection` and optional `zdr`. A data-driven Chat option dialect maps kind `routing` to top-level `provider`; Responses, Messages and unconfigured Chat routes reject nonempty options. Strict activation still waits for POR04's mandatory reasoning/tool policy.

POR04 activates the strict OpenRouter adapter. GLM-5.3-Flash exposes exact max/high/low effort with adapter-default max through `reasoning.effort`. Chat continuation preserves the exact `reasoning_details` object sequence and retains the raw `reasoning` versus `reasoning_content` alias. Tool-call ingress and response settlement require nonempty raw reasoning or details; failures publish neither provider state nor Finish. DeepSeek's independent string-only rule is unchanged. Schema v13 adds `catalog-openrouter` with `http`/`models` only to older exact configs whose LLM provider is OpenRouter. Production-loader evidence proves durable routing/default provenance and verified mock dispatch; authenticated live evidence remains the only POR04 gap.

POR06 makes OpenRouter request plugins explicit in production. `OpenRouterTransformPolicy` owns the exhaustive transform set, enable/disable wire rows, prerequisites, requested-vs-effective evidence, effects and typed costs. `OpenAiChatCompletionsConfig` now holds multiple unique provider-option dialects: routing projects its whole object to `provider`, while transforms project the sole exact `plugins` member to top-level `plugins`; missing/extra members, duplicate kinds/fields and reserved-field collisions fail before transport. Every OpenRouter constructor requires a provider-owned transform option, and the CLI always supplies the all-disabled policy. Both options persist through C02 and C05 before the exact top-level wire body is sent. Response healing remains refused on the current streaming/non-structured route, and account “Prevent overrides” keeps actual execution Unknown.

## External plugin boundary

Built-in Rust plugins remain compiled implementations. External plugins initially use safe declarative contributions:

- Skills and references.
- Commands backed by prompts or an executable protocol.
- MCP servers.
- Provider declarations over supported wire protocols.
- Hooks invoking commands, HTTP or MCP tools.
- Themes and UI metadata.
- Agent/runtime configuration.

Code-bearing external plugins use a versioned out-of-process protocol or WASI Component Model after the declarative API stabilizes. Unsafe Rust dylib loading remains rejected because it cannot provide a stable ABI or meaningful isolation.

Implemented PL01/PL02 adds low-level crate `heycode-extensions`. `ManifestValidator` parses closed TOML schema v1 into compatibility-, permission-, source- and collision-validated metadata. The Unix cache validates package bytes through held descriptor-relative traversal, content-addresses them, publishes immutable id/version refs with no-clobber commits and protects install/cleanup with OS leases. The host-neutral bridge loads bounded no-follow UTF-8 documents. Concrete `heycode-extension-host` maps them into token-owned skills, commands, agent presets, hooks, themes and providers; default `product-extensions` resolves enabled verified-cache rows during apply. PL04 canonicalizes bundled stdio command/cwd inside the immutable package root, proves executable type/mode and manifest permissions, then reuses the ordinary MCP connection owner. The lower crate performs no network I/O or publisher-signature verification. PL05 owns the marketplace boundary and verifies pinned catalog and package digests, but **not** publisher signatures, and fetches nothing. Non-Unix install remains fail-closed.

Every successful declarative host method returns an exact registration whose
one-shot withdrawal is immediately attached to Context; a host can no longer
claim success while omitting disposal. PL09 adds a strict abstract
out-of-process protocol that binds package/executable digests, deny-by-default
capability grants, correlated frames and one complete six-kind contribution
generation. Host refusal/crash/cancel tears down process and all rows. PL03/04
are production-reachable; PL09 still lacks its heycode-exec process Consumer, and
no WASI/WIT PL10 host is implied.

O05 layers background delegation without making the lower SubagentRegistry
depend on Agent. Separate `subagent-jobs` attaches weak Agent plus JobRegistry
handles after both exist. `task background:true` reserves one id/token before
spawn, returns immediately and settles completion/failure/cancellation through
the durable A03 inbox before visible state changes. U22 projects that same
registry beside committed diffs and live agent presets/providers.

E09 reuses that JobRegistry for composed shell and PTY execution rather than
inventing a second process plane. `execution-jobs` resolves the shell spec
before reserving a job, routes both launch families through heycode-exec, owns one
waiter/cancellation token and durably appends the source-attributed notice before
publishing terminal state. `/tasks`, `/ps` and `/stop` are immediate human
commands over the same owners; `background_shell` and `background_terminal`
are model tools. A concurrent terminal kill retires the id and joins the waiter,
so no process tree or settlement can race a second owner.

O10–O13 add three closed v2 session domains plus one authoritative live idle
edge. Goal snapshots are revisioned CAS with disarmed resume, admitted-round
accounting and bounded wake budget. Mid-turn plan changes commit at the next
accepted pre-step; off-turn changes commit immediately. Workflow definitions
run through a replaceable `WorkflowWorker`, checkpoint completed prefixes and
resume only after the last checkpoint. Schedules enqueue first, append a
correlated dispatch second, flush, and only then wake; recovery repairs that one
window, rearm owns every timer and fork projection ignores inherited work until
explicit new-id copy. `AgentIdle` publishes after the cancellation lease drops
while the turn gate remains held, never from renderer/spinner state.

The composition root installs `execution-jobs`, `goals`, `workflows` and
`schedules` after Agent/job ownership and supplies the explicit sequential
workflow Provider. They are optional default Consumers: no historical plugin
injects their services, so schema v25 and intentional exact profiles remain
unchanged while profile-free startup derives the expanded live default.

O06 adds an exact-base Git worktree manager and delegated-runtime subagent
Provider over the composed subprocess service. Git receives exact argv, empty
environment and disabled hooks/fsmonitor/credential helpers. Creating/ready/
retained state is owner-only journaled around side effects; cleanup is
synchronous and crash-left rows recover. The Provider still requires a full
commit id at construction, so product activation remains open until config or
Settings owns that value—composition does not silently resolve mutable HEAD.

O07 adds closed `team/change` truth and default `teams` service/tool. Root and
child authorities see only permitted roster/mail/task operations; global/task
CAS, acyclic dependencies, legal transitions and bounded waiters are enforced.
Dispatch commits team state before child input, then terminal team state before
the ordinary JobRegistry and A03 settlement. O14 similarly adds closed
`review/change`, default `reviews` and `/review-runtime`: exact tracked input
commits before an isolated worktree/delegated DenyAll run, strict findings
commit only after unchanged Git status, and every failure publishes no findings.
Both domain events are log-only in the generic transcript; their services/tool/
command are the current product Consumers.

R05/R08 share `RuntimeSubagentProvider`, an Agent-level adapter over any
permission-capable delegated `AgentRuntime`. It creates a fresh durable child,
records the provider-native session link, validates every live event through
R02, maps parent approval only to AllowOnce/Deny, refuses questions, propagates
cancellation and closes quiescently. Codex/Claude retain their own executable,
protocol and no-persistence controls; the composition root only registers the
two static provider bindings.

## Crate evolution

The likely new crates are introduced in dependency order and only when implementation begins:

| Crate | Owns |
|---|---|
| `heycode-sdk` | app-server v1 wire types and transport-neutral Rust client |
| `heycode-settings` | layered schemas, profiles, config migration |
| `heycode-credentials` | credential references, home-file provider |
| `heycode-authorization` | interactive auth flow registry |
| `heycode-models` | provider/model catalog and capabilities |
| `heycode-runtime` | native/delegated `AgentRuntime` registry |
| `heycode-app-server` | stable local JSON-RPC v1, native backend/client and effect-owned registry controls |
| `heycode-exec` | fs/subprocess/shell/terminal/LSP seams, or split when dependencies require |
| `heycode-hooks` | lifecycle hook registry and command protocol |
| `heycode-jobs` | background operations and notices |
| `heycode-extensions` | manifest v1 validator now; marketplace/install/activation and process/WASI host in later plugins |
| `heycode-extension-host` | concrete declarative registries and bundled-MCP activation above the lower package boundary |

The existing crate table in `AGENTS.md` must be amended before each crate lands, with rationale and allowed dependency arrows.

## Architecture acceptance tests

- A plugin contribution disappears after its fiber/context is disposed.
- A failing plugin causes all earlier effects to unwind in reverse order.
- A default profile contains every documented built-in capability.
- A legacy generated profile migrates without losing intentional user settings.
- A wrong concrete service type fails in a diagnostic composition test.
- Every plugin `apply` service read appears in `inject`.
- A native request matches independent session-log reconstruction.
- A delegated runtime never exposes or copies its stored OAuth tokens.
- Provider/model switching refuses incompatible opaque state with an actionable migration path.
- No CLI mode exits without `Context::shutdown()` reaching quiescence.

## 2026-08-31 integrated operation preparation and hidden media

Provider construction and provider operation preparation are distinct plugin
phases. Composition publishes an inert exact route with zero network I/O. After
catalog/model/N01 selection, Agent awaits `Provider::prepare_inference` under a
caller-owned child token; the returned operation provider is the only provider
allowed to supply options, adapter/auth preview, P10 and C02/C05 state. AWS uses
the phase for private live callability evidence; Vertex uses it for the composed
GCP profile and exact endpoint. Discovery inside streaming is forbidden.

ATT04 is deliberately not added to `ChatMessage`. A hidden exact-model/format
audio adapter consumes hash-verified ATT01 inputs only after durable association
and stages provider output until terminal success. Bytes commit before
`assistant/audio`; app-server/SDK/TUI carry metadata only. This preserves the
ordinary protocol union and prevents a shared serializer from silently dropping
or falsely advertising audio.

Installed extension activation similarly separates a manifest request from
authority. `[code]` identifies runtime/entrypoint; a managed host generation
must separately bind provenance, session, grants and resources. Six registry
adapters publish behind one retirement gate. The default root composes this
code-aware path with no authority rows, so code activation fails closed.
