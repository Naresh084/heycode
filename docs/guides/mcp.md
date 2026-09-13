# MCP servers

MCP is the supported external tool surface today. A configured server has three
separate states: a validated definition, a live connection generation, and an
atomically published tool/resource/prompt listing. Do not treat configuration
alone as a connected tool.

## Choose one transport

Each `[mcp.servers.<name>]` table selects exactly one transport.

Stdio starts a local executable through the composed subprocess and sandbox
services:

```toml
[mcp.servers.docs]
command = "docs-mcp"
args = ["--stdio"]
```

Streamable HTTP uses an absolute endpoint:

```toml
[mcp.servers.docs]
url = "https://mcp.example.test/rpc"
```

Both forms produce a real connection and real tools in the default product
root; the HTTP example is a live route, not syntax-only.

Setting both `command` and `url`, neither, or attaching stdio-only `args`/`env`
to `url` fails at config load. Server names become part of dynamic tool names:
`mcp__<server>__<tool>`.

Avoid literal credentials in `env`, URLs, command arguments, or checked-in
TOML. The MCP registry's safe snapshot can represent source kinds and
non-secret references, but that does not make a literal in the source file
safe. Use the management authorization path for supported flows.

## Two ways to declare a server, one resulting set

`[mcp.servers.<name>]` in `heycode.toml` and `heycode mcp add` are not competing
mechanisms. The product world adopts every configured row into MCP management
and then merges the management store's connectable rows into the same
connection set, so a server added through the CLI takes effect in the **next**
session and both kinds appear together in the running world.

Two rules follow. Configuration wins a name collision — a `[mcp.servers.docs]`
entry supersedes an added `docs`. And a configuration-declared server is
read-only to every mutating operation: `add`, `edit`, `enable` and `remove`
refuse it with "`docs` is declared in this session's configuration; edit its
`[mcp.servers.docs]` entry instead."

A stored (added) stdio server is a bare command with no argv and no
environment. Anything richer — `args`, `env` — must go in `heycode.toml`.

## Manage servers without a provider

The management command composes only Settings and MCP management; an unrelated
provider credential is not required. That world does not load `heycode.toml`, so
`heycode mcp list` shows only added servers while the in-session `/mcp` panel shows
both kinds.

```sh
# docs-check: syntax
management_home="$(mktemp -d)"
HEYCODE_HOME="$management_home/home" cargo run -q -- mcp list
HEYCODE_HOME="$management_home/home" cargo run -q -- mcp add docs --command docs-mcp
HEYCODE_HOME="$management_home/home" cargo run -q -- mcp test docs
HEYCODE_HOME="$management_home/home" cargo run -q -- mcp enable docs --off
HEYCODE_HOME="$management_home/home" cargo run -q -- mcp remove docs
```

For HTTP/OAuth, add the endpoint and inspect its current authorization state:

```sh
# docs-check: manual-live
export HEYCODE_HOME="$(pwd)/.heycode-live"
cargo run -q -- mcp add docs --url https://mcp.example.test/rpc
cargo run -q -- mcp auth docs
cargo run -q -- mcp test docs
```

These examples are syntax-checked only. The current top-level `auth` operation
is an inspection; it does not start a browser/PKCE callback. `auth` and `test`
require an actual server and are live evidence for that endpoint only. OAuth
connection initiation remains on the MCP05/MCP15 product-integration path.

## Project trust and process authority

Project MCP is executable/network authority. Unknown workspaces must reach an
affirmative trust decision before project definitions or local commands become
active. `--restricted-workspace` deliberately withholds that authority; a
server missing from a restricted world is not evidence that its parser failed.

Stdio children receive exact argv and an explicit scrubbed environment, run
through the mandatory sandbox policy path, and are owned as process trees.
Shutdown stops transport before removing dynamic tool rows. HTTP redirects are
not followed by the raw transport; each protocol owner must admit any authority
transition explicitly.

## Publication and replacement

Listing publication is all-or-nothing:

1. connect and initialize under one lifecycle token;
2. walk the advertised listing pages with cursor/race checks;
3. validate every candidate row and detect collisions;
4. swap the complete generation;
5. retain the previous generation during a failed refresh or reconnect;
6. retire rows before claiming that no generation remains.

A partial list never replaces last-good, and a held `ToolRegistry` loses rows
when their owning generation disposes. Dynamic registrations are still exact
plugin inventory and must not outlive their server.

## Model and UI safety

Server-authored resources, prompts, instructions, tool text, and rich results
are untrusted data. Durable results retain MCP-specific provenance and render a
fixed data-only warning; an MCP result cannot authorize a tool or bypass the
normal approval/guard pipeline merely by saying it is trusted.

Rich media is admitted through the attachment store before
`tool/rich-result` commits. Raw bytes do not enter JSONL, UI events, or Debug.

Inside the TUI, `/mcp` uses the same Settings-backed operations as the CLI.
Availability is attached to the running shell; an absent service is visible as
unavailable, not rendered as an empty healthy server list.

## Diagnose without overclaiming

```sh
# docs-check: syntax
cargo run -q -- doctor --composition --json
cargo run -q -- --restricted-workspace --fake run "list configured MCP capabilities"
```

The first proves the selected plugin graph and isolated activation with
external transports suppressed. The second proves only an offline product
turn; restricted authority may intentionally omit project servers. Neither is
a live MCP handshake.

Config fields and defaults are listed in the
[configuration reference](../reference/configuration.md).
