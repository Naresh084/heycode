# Plugins

Everything in heycode is a plugin, but two plugin boundaries must not be confused:

- Built-in Rust plugins are compiled implementations selected into a `Context`.
- External plugin manifest v1 describes declarative packages admitted through
  the extensions/cache/lifecycle boundary.

MCP is the supported external tool protocol today. A manifest is not permission
to load an unsafe Rust dynamic library, and heycode deliberately has no dylib ABI.

## Inspect the live built-in world

Inside the TUI:

```text
/plugins
/plugins verbose
```

The ordinary view opens the plugin surface. `verbose` renders the exact live
inventory: implementation source, winning activation scope, services, commands,
tools, providers, processes, UI slots, and other named rows. Broad descriptor
families say what a plugin may contribute; exact inventory says what a committed
activation actually owns.

For non-interactive graph evidence:

```sh
# docs-check: syntax
isolated_home="$(mktemp -d)"
HEYCODE_HOME="$isolated_home/home" cargo run -q -- doctor --composition --json
```

The generated [plugin reference](../reference/plugins.md) lists the composition
root's ordered candidates and manifest-v1 vocabulary. Candidate order is not a
claim that every row is live: platform, route, profile, and managed admission
filter factories before activation.

## Select built-ins with a named profile

Named profiles live at `$HEYCODE_HOME/profiles/<name>.toml`. They are strict,
ordered overlays; `--profile` and `/profile` read the same store.

```toml
schema_version = 2
name = "minimal"

[[plugins]]
id = "mcp"
enabled = false

[[plugins]]
id = "skills"
enabled = true
```

```sh
# docs-check: syntax
export HEYCODE_HOME="$(pwd)/.heycode-evaluation"
cargo run -q -- --profile minimal --restricted-workspace --fake run "describe the active plugin profile"
```

An exact profile remains exact: disabled or omitted capabilities do not
reappear through hidden defaults. Dependencies are stably topologically ordered
after scope resolution, but a missing provider, duplicate service, collision,
or cycle still fails loud.

Do not put an exact `[profile].plugins` list in root config unless you intend to
freeze both the set and order. Leaving it empty follows the current built-in
profile automatically.

## Author an external manifest

Start from the canonical generated fixture in the
[manifest reference](../reference/plugins.md#canonical-validated-fixture).
Manifest v1 is strict and contains only metadata:

- namespaced `marketplace/plugin` identity and semantic version;
- host API range and explicit target platforms;
- contribution kind/id/path/exposure;
- requested permissions;
- dependency/conflict ranges;
- source/checksum/signature metadata and update channel;
- authentication policy with non-secret credential references.

Credential values are structurally absent. Paths are normalized relative to
the package root and reject traversal, platform-specific ambiguity, case-folded
collisions, and unsafe device names.

Supported declarative contribution kinds and permissions are generated from
the closed enums; do not copy a list from this guide into tooling.

## Lifecycle commands

Plugin management composes only Settings and the plugin lifecycle boundary:

```sh
# docs-check: syntax
management_home="$(mktemp -d)"
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin list
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin install acme/code-quality 1.2.3
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin enable acme/code-quality
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin update acme/code-quality 1.2.4
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin rollback acme/code-quality
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin disable acme/code-quality
HEYCODE_HOME="$management_home/home" cargo run -q -- plugin remove acme/code-quality
```

Only `list` is expected to work in an empty evaluation home. Mutation commands
require an installed cache object and the current lifecycle's managed admission
authority; missing authority denies before mutation. These examples are
syntax-checked, not executed or advertised as a working marketplace flow.

Lifecycle state publishes only after its durable commit. Install, enable,
update, and rollback require admission; disable and remove remain recovery
operations. Update retains a prior version for directional rollback. A config
migration itself is one-way, so binary rollback can require restoring a backup
rather than reverse-migrating newer state.

## Admission and evidence boundaries

The current lower boundary can validate manifests, resolve dependency graphs,
freeze exact bytes in a Unix owner-only content-addressed cache, and enforce
managed source/channel/publisher/version/digest/signature-state/platform/
capability policy before lifecycle mutation.

That does not prove all of the following:

- remote package discovery or fetch;
- publisher signature validity (presence is not verification);
- upstream checksum validity when it addresses a different artifact;
- concrete activation of every declarative contribution kind;
- non-Unix cache owner-security equivalence;
- a marketplace, update channel, or release installer.

Those gaps stay explicit in the [engineering tracker](../engineering/TASKS.md).
The activation transaction and generation rules are detailed in
[plugin lifecycle](plugin-lifecycle.md), with a matching diagram.
