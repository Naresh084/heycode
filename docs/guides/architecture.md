# Architecture

heycode is a composition root around a plugin kernel. The binary may parse process
arguments, choose a profile, and construct every concrete crate, but provider
logic, tools, commands, persistence, UI, policies, and the agent loop enter the
world as plugins and services.

[Open the architecture overview diagram](diagrams/architecture.html). It uses
the default Diagram Design editorial skin, `doc-wide` size, simplified detail,
and engineer-oriented labels. It intentionally collapses the many concrete
registries and providers into seven nodes.

## The three-role completeness rule

A capability is product-complete only when all three roles exist:

| Role | Responsibility | Example |
|---|---|---|
| Service definition | Stable interface, typed values, errors, and ownership boundary | `ToolRegistry`, `CatalogRegistry`, `SessionQueryService` |
| Service provider | Concrete implementation, configuration, cancellation, and disposal | local filesystem, reqwest HTTP, JSONL session query |
| Consumer | A current model-, CLI-, TUI-, runtime-, or app-server path that uses it | `read`, `/model`, `/resume`, app-server controls |

A correct library type without a service key is not reachable. A registered
service without a Consumer is substrate, not a product feature. A UI fixture
without production service attachment is not a command path.

## Composition flow

1. The CLI discovers root config and an optional named/scoped profile.
2. `PluginFactories` constructs the selected concrete plugin instances.
3. Profile precedence and managed admission decide which rows may exist.
4. A stable service-dependency order places providers before Consumers while
   preserving independent order.
5. `compose_scoped_activation` activates each plugin transactionally.
6. A successful `Context` publishes services, exact inventory, events, and
   effect-owned registrations.
7. Run modes resolve only typed owner constants from that context.
8. `Context::shutdown()` unwinds effects LIFO before process exit or world
   replacement.

Relevant code contracts:

- [`Plugin` and composition](../../crates/heycode-core/src/plugin.rs)
- [`Context` service/effect ownership](../../crates/heycode-core/src/context.rs)
- [verified activation transactions](../../crates/heycode-core/src/activation.rs)
- [exact contribution inventory](../../crates/heycode-core/src/inventory.rs)
- [composition root and built-in candidates](../../crates/heycode-cli/src/lib.rs)

## What `Context` owns

`Context` is intentionally small:

- a type-erased service map keyed by owner-defined `ServiceKey` constants;
- successfully applied plugin descriptors and activation scopes;
- exact named contribution inventory;
- an effects stack whose disposers run LIFO;
- a typed cross-plugin `EventBus`.

`ServiceKey` does not encode the Rust value type. `ctx.get::<T>(key)` returns
`None` for the wrong `T`, so composition diagnostics and owner constants remain
part of the contract. Duplicate keys, missing injects, descriptor/name drift,
and exact contribution collisions fail during activation.

## Four data planes

The architecture keeps four related planes distinct:

1. **Durable model plane.** Session JSONL is the authority for model-visible
   history and any provider state needed for replay.
2. **Provider plane.** Exact route/catalog/credential resolution builds an
   ownership-consumed call, then independently verifies it against the log
   before transport.
3. **Human plane.** Slash commands, dialogs, panels, and live `UiEvent`s do not
   become model input merely because they are visible.
4. **Operational plane.** Services, processes, Settings generations, MCP rows,
   background work, and telemetry are lifecycle-owned effects.

Crossing from one plane to another requires an explicit owner and commit point.
For example, composer text is not a user message until the Agent claims it and
the session commits `user/message`; rich media bytes commit before their
metadata event; a Settings CAS commits before live route publication.

## Events and interception

`EventBus` is synchronous notification after a fact is already committed.
Session listeners see a line only after write and flush. `UiEvent` is live and
non-durable; replay-safe UI rebuilds content from session events.

`Waterfall<T>` is async around-middleware for decisions. A layer delegates via
`next.run(input).await`; returning without `next` is a deliberate
short-circuit. Global decisions live on global services, while one-Agent
step/request seams remain owned by that Agent so subagents do not inherit
another loop's mutable policy state.

## Dependency direction

`heycode-core` imports no internal crate. Neutral shared vocabulary moves down to
the lowest common owner; session projection emits provider-neutral messages,
and `heycode-llm` never imports session types. `heycode-agent` joins them. Only the
binary may know every crate.

This direction is why session facts are neutral and provider state is tagged
with provider/model/protocol identity rather than implemented as a dependency
from session to a specific adapter.

## Diagnose the selected world

```sh
# docs-check: syntax
diagnostic_home="$(mktemp -d)"
HEYCODE_HOME="$diagnostic_home/home" cargo run -q -- doctor --composition --json
```

The graph phase is zero-apply. The activation phase uses disposable state, fake
inference, no watcher/resume, and suppressed configured MCP transports. Its
result is architecture evidence, not live credential/provider/MCP health.

## Detailed companions

- [Plugin lifecycle](plugin-lifecycle.md) explains activation, rollback,
  shutdown, and generation replacement.
- [Session v2](durable-sessions.md) explains append-only truth and exact route-aware
  projection.
- Generated [plugins](../reference/plugins.md),
  [commands](../reference/commands.md), and
  [configuration](../reference/configuration.md) pages keep volatile descriptor
  inventories out of this prose.
