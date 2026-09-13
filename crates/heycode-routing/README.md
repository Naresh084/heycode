# heycode-routing

`heycode-routing` owns the Settings-backed effective runtime/provider/model tuple
and its human command Consumers. Validation uses live provider/model/runtime
registries. Settings CAS commits before Agent publication; an invalid or stale
transition cannot update the live route.

## Active model and effort ownership

`RoutingService::active_configuration()` distinguishes the active control
owner from the dormant native fallback. Native sessions expose their inference
provider/model/effort; delegated sessions expose their runtime and optional
`runtime_model` without relabeling the saved native provider as active.

Native `/effort` choices come from the selected provider adapter and exact
model descriptor. The command is available only when that route publishes a
non-empty, duplicate-free vocabulary. Selection validates an exact value,
commits it through Settings CAS, then atomically publishes provider, model and
effort to `Agent`. Model changes clear the old effort; runtime changes preserve
the native fallback tuple. Picker submissions retain their original typed owner
and routing revision, and fail without mutation if backend ownership or its
configuration changed. Delegated application remains unavailable until its
runtime control bridge is mounted.

## Command-line pins outrank the persisted route

`routing_plugin_with_overrides(RoutingOverrides)` layers the fields this
process's command line pinned (`--provider`, `--model`, `--set llm.*`) as the
Settings `Override` layer: above the persisted user/project route, below
managed locks. Without it a `--model x` was silently lost to yesterday's
`/model` in `settings.toml`. The override is ephemeral — the first in-session
`/model` or `/provider` write drops it — so `/model` still works after
`--model`. `RoutingService::override_notice()` names what the flag displaced
(`command line sets model \`x\` for this session (user settings have \`y\`)`)
and the TUI shows it once at startup.

## Opaque compaction switches

C14 refuses a direct provider or model transition when the durable session's
winning compaction settlement is provider-native and belongs to another route.
The error names three explicit command choices:

- `/provider <id> portable` asks the current provider for a portable summary,
  verifies it superseded the native marker, then commits Settings/live route;
- `/provider <id> fork` creates a durable shared-prefix child immediately
  before native settlement and leaves the current route unchanged;
- `/provider <id> cancel` changes nothing.

`/model <id>` accepts the same second argument for a model-only route change.

Equal compaction boundaries are last-write-wins, so the later portable marker
can resolve a native checkpoint without rewriting JSONL. An app-server request
that cannot express a choice receives Conflict. Initial/external Settings apply
uses the same barrier and fails live publication rather than bypassing policy;
incompatible native state also remains excluded by session projection.

## Focused verification

```sh
cargo clippy -p heycode-routing --all-targets -- -D warnings
cargo test -p heycode-routing
```

## Connection setup update — 2026-09-05

Connection setup stages a provider/model target for the next composition without publishing it as the live Agent route. Targets use provider-owned profiles and exact catalog evidence; the default remains available when discovery fails. The composed provider activates a pending selection after admission. Runtime model choices persist separately from native provider models. Optional fields are omitted from TOML rather than serialized as null.

ConnectionProfile carries optional model defaults and local/hosted categories. Local selections require catalog evidence. Saved intent is reported independently of credential readiness, and disconnected placeholders cannot erase owned credential references.

Draft model selection stages a validated credential-free HTTP(S) endpoint with the model in one pending connection write. Startup restores both; model/runtime changes retain the native endpoint. Optional fields are omitted from persisted TOML.

Routing persists an optional typed credential reference with endpoint/model in one pending connection. References have explicit public Settings roles; values remain in the credential service. Model/runtime changes retain the reference.

Staging also checks the connection profile’s adapter model scope. A discovered model cannot bypass an exact adapter restriction, and rejection occurs before Settings are written.

Complete staged selections retain bounded non-secret region/project/location/deployment coordinates. Startup and Settings validate their shape; model/runtime changes preserve them. Provider composition validates which coordinate names it consumes. Amazon Bedrock's region form now stages its coordinate, discovered model and credential reference through this single pending-selection transaction.
