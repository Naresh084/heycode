# Model and effort routing contract

Status: implemented across routing, native adapters, runtime/app-server control,
session replay and TUI ownership. Individual runtimes still enforce their
advertised configuration capabilities.

## Problem

`/model` currently projects the dormant native inference route even when a
delegated runtime owns the active loop. That sends Claude sessions through the
OpenRouter catalog and lets the UI claim a change that the active backend never
received. `/effort` has the inverse problem: native adapters already validate
and serialize exact effort values, but routing cannot discover or persist them.

The active execution backend must own discovery, validation and application.
The native provider/model/effort tuple remains durable while a delegated
runtime is active, but it is a dormant fallback and must not be presented as
the active model.

## Required backend API

The runtime/app-server owner should expose one typed, object-safe control plane
with no provider-name or runtime-name branching in the TUI:

```text
active_configuration() -> BackendConfiguration
models(refresh, cancellation) -> BackendModelCatalog
efforts(cancellation) -> BackendEffortCatalog
apply_model(model, cancellation) -> BackendConfiguration
apply_effort(effort, cancellation) -> BackendConfiguration
```

`BackendConfiguration` needs the active `runtime`, the loop owner
(`NativeInference` or `DelegatedRuntime`), the displayed model, optional effort,
and a generation/revision suitable for rejecting stale picker submissions.

`BackendModelCatalog` needs backend identity, current model, safe freshness or
error evidence, and selectable model rows. It may reuse `CatalogSnapshot` when
the runtime can truthfully produce that shape. A delegated runtime catalog must
come from `AgentRuntime::models` (or the live session's equivalent), never from
the dormant native `CatalogRegistry` provider.

`BackendEffortCatalog` needs exact accepted IDs in display order, the current
selection, and the backend-owned default. An empty catalog means unsupported;
unknown is not an empty supported list.

Application must validate against the same live backend generation that
produced the picker and must update the running backend before reporting the
new configuration. Persist only after runtime application succeeds, or use a
backend-owned prepare/commit protocol that can prove the same atomic result.
Failure leaves both the running configuration and durable routing unchanged.

## Native adapter side

`InferenceAdapter` exposes its exact `ReasoningEffortOptions` for a resolved
model. The default is unsupported. Provider adapters return the values already
held in their request configuration (`ResolveSpec.reasoning_efforts` and
`default_reasoning_effort`); the routing layer does not hard-code providers or
binary effort names.

Native routing validates the selected model through the current catalog,
queries the selected provider adapter for its effort options, performs the
Settings CAS, then publishes model/effort to `Agent`. `Agent` copies the
selected effort into `RequestDraft` before request interception, durable C02/C05
snapshotting and provider transport. Unsupported choices fail before Settings
or live state changes.

## Durable shape and resume

- `provider`, `model` and `effort` are the native fallback tuple.
- `runtime_model` and the runtime lane's corresponding effort field are the
  delegated tuple. Selecting either must never rewrite the native tuple.
- The current top-level `runtime` chooses which tuple is active.
- Resume/recomposition must pass the selected delegated values into the runtime
  start/resume/backend configuration path before the next turn.
- Status, `/model`, `/effort`, and picker refresh all read
  `active_configuration`; none reconstruct active state from dormant fields.

## TUI integration boundary

The TUI state machine records a typed picker owner (`NativeInference` or
`DelegatedRuntime`) with each refresh and selection request. The event loop
dispatches native requests to `CatalogRegistry`/`RoutingService` and delegated
requests to the backend control API above. A response whose owner/generation no
longer matches the open picker is discarded as stale. Loading, ready,
unsupported and error remain distinct states.

`RoutingService` owns both branches behind the typed picker owner. Delegated
discovery calls the active `AgentRuntime`; delegated application calls the live
app-server backend, verifies the returned effective model/effort, then commits
the separate durable runtime tuple. Missing controls, runtime identity changes,
stale revisions and backend failures return an explicit error without falling
back to a native provider catalog or publishing a durable selection.
Each successful wire update carries the accepting app-server backend generation.
If effective-response validation or the subsequent Settings CAS fails, routing
retires that exact session generation and requires it to reopen from the durable
tuple. A delayed cleanup cannot detach a newer replacement backend.

The concrete live discovery sources are runtime-owned. Claude reads model and
effort options from its initialize handshake; Codex reads the supported app
server model APIs; OpenCode and Grok use ACP session model/config options.
DeepSeek Harness advertises no model discovery, so model picking remains
unsupported there. ACP runtimes reject custom system prompts and host tool
catalogs; Grok leaves reasoning-effort support unknown unless its live ACP
process advertises a thought-level option. These capability limits surface as
unsupported or unavailable instead of being synthesized by routing.

## Acceptance checks

1. With runtime `claude`, opening `/model` records only a Claude runtime model
   discovery request; the native catalog request count remains zero.
2. Applying a delegated model changes the active backend and delegated durable
   field while preserving native provider/model/effort.
3. A native effort advertised by the adapter reaches the durable request header
   and exact provider request body.
4. An unsupported effort changes neither Settings nor `Agent`.
5. Restart/resume restores the active tuple and the first subsequent request
   retains the configured model, effort, system prompt and tools.
