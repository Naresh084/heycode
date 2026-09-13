# Source-checkout setup

heycode currently has no certified installer or fresh-machine release matrix. The
reproducible path is a source checkout with a Rust toolchain. This guide proves
only local deterministic startup unless a step is explicitly labelled live.

## Prerequisites

- A checkout of this repository.
- A Rust toolchain capable of building the workspace.
- A supported terminal for the full TUI, or `--screen-reader`/`TERM=dumb` for
  flat output.
- Provider credentials only when you deliberately run a live step.

No ordinary documentation check reads the real heycode home, keychain, provider
credential, or network.

## Build from the saved checkout

From the repository root:

```sh
# docs-check: syntax
cargo build --release -p heycode-cli
./target/release/heycode --restricted-workspace --fake run "reply with: heycode is ready"
```

The release build is syntax-checked by the docs gate but not rebuilt there; the
gate executes the debug-path offline smoke below to avoid duplicating a full
compile.

## Run the isolated offline smoke

This is the one guide example executed by `scripts/verify_docs.py`. It uses a
new temporary home, the fake provider, and restricted workspace authority.

```sh
# docs-check: run
docs_home="$(mktemp -d)"
HEYCODE_HOME="$docs_home/home" cargo run -q -- --restricted-workspace --fake run "reply with: offline smoke passed"
```

Expected evidence is a completed fake-provider turn. It proves local config
discovery, composition, session append/replay, the native agent loop, and
shutdown in this checkout. It does not prove a provider credential, MCP server,
external plugin, delegated runtime, installer, or platform sandbox.

## Choose workspace authority deliberately

- `--restricted-workspace` opens without project executable/settings
  authority. Use it for first inspection and documentation examples.
- `--trust-workspace` grants project authority for this process. Review the
  checkout before using it.
- Interactive startup with neither flag opens the typed trust decision before
  project configuration, skills, hooks, or MCP are loaded.

Headless examples should always state one of the two flags; an omitted trust
decision is not a portable automation contract.

## Keep product state isolated while evaluating

`HEYCODE_HOME` must be absolute. It owns root config, Settings, credentials,
catalog cache, profiles, sessions, plugin state, and retained health. A relative
value fails before it can become project authority.

```sh
# docs-check: syntax
evaluation_root="$(mktemp -d)"
export HEYCODE_HOME="$evaluation_root/heycode-home"
cargo run -q -- doctor --composition --json
cargo run -q -- --restricted-workspace --fake run "inspect this checkout"
```

The composition doctor performs a zero-apply graph inspection plus isolated
activation with explicit suppressions. It is not credential, provider, or MCP
health evidence.

## Start the interactive shell

```sh
# docs-check: syntax
cargo run -q -- --restricted-workspace
cargo run -q -- --restricted-workspace --screen-reader
```

With no usable credential, the first command opens descriptor-driven setup and
keeps the composer blocked until authorization commits and the world
recomposes. The screen-reader form uses the same interaction state without an
alternate screen, animation, color, or cursor addressing.

Continue with [provider/runtime setup](providers.md), [MCP](mcp.md), or
[plugins](plugins.md). Exact root config keys and defaults are generated in the
[configuration reference](../reference/configuration.md).

## Evidence still required outside this guide

- Signed installer and rollback behavior are owned by Q14.
- A real new-user turn on macOS, Linux, and Windows is owned by Q16.
- Windows persistent trust and restrictive filesystem confinement remain
  fail-closed gaps.
- A successful fake turn never promotes a provider/runtime route to live or
  supported status.
