# heycode-extension-host

Concrete product adapters for declarative and installed-code extension packages.

The lower `heycode-extensions` crate validates manifests, cache identity,
permissions and immutable documents without depending on product registries.
This crate sits above that boundary and maps strict documents into live skills,
slash commands, subagent presets, hooks, themes, OpenAI-compatible providers
and plugin-bundled stdio MCP servers.

Every registration returns an exact token-owned disposer. The aggregate
`product-extensions` plugin attaches those disposers to the shared `Context`;
MCP transport teardown kills the child before withdrawing its generated tools.
Provider credentials are references resolved per operation and never values in
package documents or diagnostics.

Bundled MCP commands are normalized relative paths inside the verified PL02
object. Activation canonicalizes the root and target, rejects escapes and
non-executable files, and passes an absolute executable plus exact arguments to
the composed subprocess/sandbox service. Package permissions must include
`mcp_connect` and `process_spawn`; credential-backed environment entries also
require `credential_use`.

## PL09 code-plugin host transaction

`ProductCodePluginHost` is the concrete cross-registry transaction above the
host-neutral protocol. It requires exactly one adapter for each code-capable
PL03 kind (skill, command, agent preset, hook, theme and provider); MCP remains
owned by the ordinary PL04 connection path. Adapters register proxies while a
shared `CodePluginInvocation` gate is closed. The runtime commits that gate only
after it owns the complete generation, and withdrawal closes it before removing
registrations in reverse order. Partial refusal and adapter panic roll back the
prepared prefix without ever making it callable.

The production adapter set registers the real `SkillSet`, command,
subagent-preset, hook, theme and provider rows. Immutable skill/preset/hook/theme
metadata comes from documents frozen with the exact package tree. Commands and
the provider route use bounded invocation proxies; dropped calls cancel and
join their worker rather than detaching it. Provider output is a closed
normalized chunk sequence with exact terminal/usage/tool-fragment validation.
Every row is withdrawn through its real token-owned registry handle.

`HeycodeExecCodePluginLauncher` is the concrete native launcher. It passes the
frozen executable bytes and digest to heycode-exec's exact-image seam, supplies an
explicitly empty environment, maps the manifest grant ceiling onto filesystem,
network and descendant authority, and runs one big-endian length-prefixed JSON
exchange driver over the common raw stdio path. Frames remain capped at 1 MiB;
one exchange gate preserves request/response order; caller cancellation,
hostile framing, EOF and process failure retire the generation. Shutdown joins
the driver only after heycode-exec has reaped the process tree. The executable
image lease remains attached to that tree through settlement, closing the
spawn-before-exec deletion race.

The real integration fixture fragments responses byte-by-byte, creates a live
descendant during cancellation, and activates all six real product domains
behind one gate. `installed_product_extensions_plugin_with_code` and its lazy
root variant preserve the existing `product-extensions` identity while
classifying every enabled lifecycle row. A manifest `[code]` package must match
one explicit `InstalledCodePluginAuthority`; missing/duplicate/version/runtime/
session/grant/resource facts fail before publication. The old declarative-only
installed path refuses code packages instead of silently activating their
documents without the process.

`ManagedInstalledCodePluginAuthorityProvider` is the production
`InstalledCodePluginAuthorityProvider`. It joins a fingerprinted
`ManagedPluginAdmissionGeneration` to the typed profile-v3
`ManagedCodePluginAuthorityGeneration`. Construction is zero-I/O and rejects a
stale PL08 fingerprint. Resolution runs inside plugin apply, receives the lazily
opened cache and enabled lifecycle snapshot, calls
`prepare_managed_declarative`, verifies exact version/tree
digest/runtime/entrypoint/grants/resources, mints a fresh
`CodePluginSessionId`, and returns only
`InstalledCodePluginAuthority::from_managed` rows.

The exact root constructor handoff is:

```rust,ignore
let installed_code_authority = Arc::new(
    heycode_extension_host::ManagedInstalledCodePluginAuthorityProvider::new(
        managed_plugin_admission_generation.clone(),
        managed_code_plugin_authority_generation,
    )?,
);

heycode_extension_host::installed_product_extensions_plugin_from_root_with_code_provider(
    product_extension_cache_root,
    product_extension_validator,
    installed_code_authority,
)
```

The lifecycle owner must receive the same first value through
`ManagedLifecycleAdmission::from_generation`. The root maps the already
source-checked `heycode-config` profile rows into the host rule constructors; it
does not reparse ambient bytes. No plugin id, service key, root config schema or
factory-inspection I/O is added. With no trusted managed profile generation,
the existing empty provider remains the fail-closed product behavior.

## PL10 Wasmtime Component host

`WasmtimeWasiComponentEngine` links Wasmtime 48.0.1 with its WASIp3 host. It
validates the real Component binary, exact `dshx:code-plugin/plugin@1.0.0`
export and typed WIT functions, then runs the ordinary PL09 handshake and
invocations on one owned Store thread. Store memory, tables, instances and fuel
are bounded. Epoch interruption is instance-scoped even though Wasmtime epochs
are engine-global: a cancellation marks only that Store, while other Stores
advance their deadlines and continue.

The WASI context starts empty. Only policy-minted preopens and exact IP+port TCP
destinations are enabled; UDP, listening, acceptance, ambient DNS, environment,
arguments and stdio inheritance remain denied. Write-only and hostname grants
fail rather than becoming read-write or global DNS. Checked-in pure and scoped
components prove real Canonical-ABI typechecking, initialization, invocation,
WASI linking, world mismatch refusal, malformed component rejection and fuel
termination.

The ABI report records the Component's actual import subset rather than the
whole authority ceiling. That set must remain within the selected pinned WASI
0.3.1 world, while the exported interface, function types, WIT digest and world
identity remain exact. The combined installed factory selects this runtime only
from manifest metadata and accepts no implicit preopen, endpoint or ambient
host resource.

PL11 still exposes only the curated lower projection. This crate does not
register an inspector: QSEC01 review and root-owned Tool/Agent durability wiring
remain prerequisites, exactly as the tracker requires.

## File-authored hooks and agents

A user should not need to author, install and enable a plugin to add one hook
or one agent. `user_declarations` reads the same JSON documents plugins
contribute — `hooks/*.json` and `agents/*.json` — from `$HEYCODE_HOME` (always)
and from the trusted workspace's `.heycode/` (under the instructions gate; a
project hook whose action runs a command additionally needs process
authority). The `product-extensions` plugin registers them as context effects
with owners `user:<file>` / `project:<file>` and preset ids
`user-<file>` / `project-<file>`, and contributes each as a dynamic inventory
row. An invalid file is a skipped row with a reason, never a refusal to start.

## Installing a declarative package from a directory

`heycode plugin install <directory>` verifies the package into the cache and
installs it. `DeclarativeUserPluginPolicy` admits every operation for a package
that ships no `code` section — the user's own declarative configuration — and
still requires managed policy for packages that do, saying so in the error.


## Custom agents: author, validate, import and reload

The installed product host loads `$HEYCODE_HOME/agents/*.json` and trusted
`<workspace>/.heycode/agents/*.json`. Files use this strict shape:

```json
{
  "display": "Repository reviewer",
  "description": "Review a specified module for correctness defects.",
  "instructions": "Inspect source and tests. Report actionable defects with locations.",
  "provider": "native",
  "mode": "oneshot",
  "config": {
    "permissions": "read_only",
    "tools": ["read", "glob", "grep"],
    "denied_tools": [],
    "skills": [],
    "mcp_servers": [],
    "max_turns": 32,
    "memory": "session",
    "isolation": "shared",
    "background": false
  }
}
```

`provider` selects a delegation runtime; `config.inference_provider` selects a
registered inference provider inside the native runtime and requires an
explicit `config.model`. Optional `model` and `effort` otherwise override the
inherited native route. Effort is validated against the selected adapter's
capabilities before inference. A runtime that cannot enforce the configuration
refuses it before starting; runtime names do not imply configuration parity.

Modes are `oneshot`, `continuable`, `fork`, and `fork_continuable` (fork with
retained follow-ups). Missing tool/skill/MCP lists inherit their respective surfaces. Empty lists
allow none. Lists contain exact names, denial wins, and MCP references select
already configured servers; they do not start or reconfigure connections.
`skills` controls the advertised catalog and `load_skill` arguments. These are
tool permissions, not a filesystem secrecy boundary: a child allowed arbitrary
file reads can still inspect files it can access.

Permissions are `inherit`, `read_only`, `default`, `accepted_edits`,
`full_access`, or `deny`. Every mode retains the parent's approval and tool
policy ceiling. `full_access` adds no permission beyond the parent. `default`
requires a fresh human decision, and `accepted_edits` uses child-local remembered
exact-action grants; both deny calls on surfaces without a prompter. Read-only
uses a closed list of local inspection tools, including no shell execution.
Narrowed presets disable hosted tool routes, inherited lifecycle hooks, and
captured orchestration tools whose services could escape the child ceiling.

`max_turns` is a limit of 1–10,000 inference steps across the retained child's
entire lifetime, including later `send_message` calls. Reaching it records a
durable `max_steps` settlement. Instructions occupy the child's provider-neutral
system instruction slot; the user task remains byte-exact user input.

`memory` can be `session` (no cross-session notes), `user`, `project`, or `local`.
Persistent notes use a stable source preset identity, even when invoked through
an alias. User notes live under `<sessions>/.agent-memory/user/<preset>/`;
private project notes use `<sessions>/.agent-memory/local/<workspace-hash>/<preset>/`;
project notes use `<workspace>/.heycode/agent-memory/<preset>/`. The
`agent_memory_read` and `agent_memory_write` tools read/replace `MEMORY.md` with
an exact revision check and a 64 KiB bound. Parent and preset permissions still
apply to writes. Missing notes are read without creating files or directories.
Read-only agents can consume existing notes but cannot update them.

`isolation: "worktree"` requests the native worktree lifecycle owner. It must
refuse if no manager is configured; it never silently runs in the shared
workspace. Integration of that manager, result preservation and continuable
worktree lifetime belongs to the runtime lifecycle workstream.

A `reviewer.json` file has `user-reviewer` or `project-reviewer` as its explicit
id. The bare `reviewer` alias selects project over user over the built-in
fallback. Definitions replace the whole preset; fields are not merged across
scopes. File names must be kebab-case. Files beginning with `user-` or
`project-` remain available through their full scope-qualified ids; they get no
bare alias because those prefixes reserve the explicit namespaces. Unknown fields, invalid values, unreadable files,
symlinks and documents exceeding 1 MiB are diagnostic rows at startup.

Human controls are available through `/agent-config`:

- `list` shows active ids and diagnostics; `show <id>` shows resolved metadata.
- `create <name>` writes a safe initial user JSON file without overwriting.
- `save <name> <JSON>` validates and atomically replaces a user definition.
- `validate <path>` checks native JSON without publishing it.
- `import <claude|codex> <name> <source-path>` converts and validates a foreign
  definition, then creates a user file without overwriting.
- `reload` validates all agent files before replacing the owned generation
  atomically. Any invalid agent or foreign ownership conflict preserves the
  previous generation. Running children retain their resolved settings;
  subsequent spawns use the new generation. Removing a project file and
  reloading restores the user or built-in fallback.

UI consumers use `AgentDeclarationService` under `SERVICE_AGENT_DECLARATIONS`
for save/reload/diagnostics. Context disposal withdraws commands and the exact
preset generation and makes retained service handles terminal.

Imports are deliberately strict. Claude Markdown imports name, description,
body instructions, exact model ids, effort, mapped tool names, plan/default
permission modes, maxTurns, memory, isolation, background and string MCP server
references. Model aliases need an explicit native model id. Claude hooks,
skill *preload* semantics, inline MCP definitions, initial prompts, permission
modes without equivalent native semantics and unknown fields are refused.
Codex standalone TOML imports name, description, developer_instructions,
model, model_reasoning_effort and read-only sandbox mode. Other sandbox modes,
inline MCP configurations, skill path configuration and unknown fields are
refused rather than discarded. The imported description is retained. YAML
include/property features are disabled in the parser.

Format references checked for the implementation:
[Claude subagents](https://code.claude.com/docs/en/sub-agents) and
[Codex subagents](https://developers.openai.com/codex/subagents).
