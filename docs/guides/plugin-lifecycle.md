# Plugin lifecycle

Plugin activation is a transaction over one `Context`, and runtime teardown is
an explicit LIFO operation. Dropping a `Context` allocation does not execute its
stored disposer closures.

[Open the activation state-machine diagram](diagrams/plugin-lifecycle.html). It
uses the default editorial skin, `doc-inline` size, simplified detail, and an
engineer audience. Reload generations and external package admission stay in
the prose so the state machine remains within its transition budget.

## Contract

Every plugin declares five things before it mutates a world:

```rust
fn name(&self) -> &'static str;
fn descriptor(&self) -> PluginDescriptor;
fn inject(&self) -> &'static [ServiceKey];
fn provides(&self) -> &'static [ServiceKey];
fn inventory(&self) -> Vec<PluginContributionSpec>;
fn apply(&self, context: &mut Context) -> CoreResult<()>;
```

`inject` is the runtime dependency contract. `provides` is the side-effect-free
composition plan. `inventory` declares exact static non-service rows; service
ownership is attributed automatically by `Context::provide`, while genuinely
dynamic rows use `Context::contribute` during `apply`.

## Activation stages

| Stage | Checks or work | Publication rule |
|---|---|---|
| Admission | descriptor id equals name, plugin id unique, every inject already present, transaction opens | no contribution is committed |
| Declaration | exact static inventory rows register | collision aborts and rolls back |
| Apply | plugin publishes services and effect-owned contributions | candidate state remains inside the open transaction |
| Commit | descriptor and winning scope record after `apply` succeeds | plugin becomes a live row |
| Rollback | remove transaction services/rows, unwind its new effects, recapture fingerprint | residue becomes `BrokenActivation` |

The fingerprint covers services, exact inventory, recorded plugins,
descriptors, scopes, pending effects, and listener count. Rollback is accepted
only when the post-rollback fingerprint matches the pre-activation one.

If one plugin fails, later rows are `not_attempted`; composition shuts down the
whole partial context LIFO before returning the original error. A rollback that
leaves residue reports the plugin, stage, residue class, and original cause
instead of pretending the world was restored.

Source contracts:

- [composition and stage ordering](../../crates/heycode-core/src/plugin.rs)
- [transaction fingerprint and diagnostics](../../crates/heycode-core/src/activation.rs)
- [service/effect rollback](../../crates/heycode-core/src/context.rs)
- [exact inventory ownership](../../crates/heycode-core/src/inventory.rs)

## Effects and shared registries

Anything a plugin installs must have one lifecycle owner: a service, listener,
command, seam layer, temporary root, process, registry row, or worker. Related
registrations belong in an effect order that makes their reverse teardown safe.

The activation transaction can directly remove only state the `Context` owns.
A contribution into an earlier plugin's shared registry therefore needs its own
disposer:

- `CommandRegistry::register_effect` for late commands;
- `EventBus::on_effect` for listeners;
- `Waterfall::push_effect` for interception layers;
- token/handle-owned removal for dynamic MCP tools and similar generations.

The `_shared` forms are registry-lifetime operations for the registry's own
owner. Using one from another plugin creates state the generic transaction
cannot see or withdraw.

## Shutdown

`Context::shutdown()`:

1. marks the context closed;
2. takes the entire disposer stack;
3. invokes disposers in reverse registration order;
4. contains a panicking disposer so later cleanup still runs;
5. is idempotent.

Run modes and higher owners must call it. A child process being kill-on-drop is
not a substitute for withdrawing listeners, commands, services, and other
effects in the correct order.

## Reload generations

`GenerationRegistry` composes a candidate before touching the live world.

- A failed candidate is already unwound and the existing generation stays
  available without consuming a generation number.
- A successful candidate swaps once.
- In-flight readers hold `Arc<GenerationContext>` for their exact old world.
- Retired worlds park until their last reader releases them.
- `GenerationContext::drop` is the terminal owner that calls
  `Context::shutdown()`.

This is compose-before-retire, not stop-then-hope. A host that never wires the
generation registry still has a correct library boundary but does not thereby
claim product hot reload.

See [generation ownership](../../crates/heycode-core/src/generation.rs).

## External package lifecycle is an additional gate

External manifest/cache/lifecycle state sits before Rust plugin activation:

1. validate strict manifest and portable paths;
2. resolve dependency/platform constraints;
3. admit current source/provenance/policy evidence;
4. freeze and recheck exact package bytes;
5. commit install/enable/update/rollback state;
6. invoke a concrete declarative host;
7. activate resulting Context contributions transactionally.

A manifest, cache object, or enabled Settings row alone skips later gates and
is not an active plugin. Managed policy is mandatory for install, enable,
update, and rollback in the product lifecycle; missing authority denies before
mutation. Disable and remove remain recovery operations.

Current manifest fields and built-in candidates are generated in the
[plugin reference](../reference/plugins.md). Current product gaps include
remote fetch, publisher signature verification, concrete hosts for every
declarative kind, marketplace distribution, and equivalent non-Unix cache
security.

## Operational checks

```sh
# docs-check: syntax
cargo run -q -- doctor --composition --json
cargo run -q -- plugin list
```

The doctor distinguishes graph inspection from isolated activation. `plugin
list` reads lifecycle state and does not prove that a package's contributions
are mounted in the current product world. Use `/plugins verbose` for exact live
inventory and preserve those evidence classes separately.
