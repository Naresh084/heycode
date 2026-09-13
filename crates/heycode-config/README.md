# heycode-config

`heycode-config` owns fail-loud configuration discovery, root schema migrations,
scoped plugin profile resolution and the strict standalone named-profile store.

Root documents currently use schema v29. Standalone profile documents use
schema v3: schema-v1 plugin rows and schema-v2 managed constraints remain
readable, while v3 adds managed installed-code authority. Migrations preserve
intentional exact profiles and insert only dependencies required by a Consumer
already present.
The v20→v21 step inserts `profiles` and `commands` before `tui`; profiles without
TUI are unchanged. The v21→v22 step inserts `compactions` before the first
`agent` or `subagent`, because both now inject that strategy registry.
The v22→v23 step inserts `session-query-jsonl` before `tui`, whose U15/CMD06
session browser and lifecycle commands now require that replaceable owner.
The v23→v24 step inserts `settings` before OpenAI/Anthropic policy owners and
repairs counter-only Anthropic profiles with their authorization/settings/
provider owner chain.
The v24→v25 step inserts `subagent-jobs` only into profiles that already carry
the owning Agent/subagent stack, and inserts the Codex/Claude delegated
subagent bridge only when that exact runtime is selected.
The v25→v26 step admits optional `subagent.worktree_base` as a full exact Git
commit. Existing documents add no field and preserve every plugin choice; the
version boundary prevents an older reader from silently ignoring requested
worktree isolation.
The v26→v27 step adds `llm.protocol` (`auto|openai_chat|openai_responses|
anthropic_messages`) and optional positive `llm.max_output_tokens`. Existing
documents retain `auto` and no output override; older readers refuse a
route-defining dialect instead of silently dispatching another protocol.
The v27→v28 step inserts the Settings-backed OpenAI/Anthropic policy plugin
before `llm` only for those selected providers, and inserts the AWS/Google
policy-namespace owner before an exact cloud inference row. This keeps one
registered Settings snapshot authoritative for both provider request policy
and N01 candidates; an omitted policy owner never turns into a hidden default.

The v28→v29 step retires `credentials-keychain` from complete profiles and
retains exactly one `credentials-file` row. It preserves unrelated config and
backs up the original document before commit; it never opens an OS credential
store. Current or named profiles explicitly enabling the retired plugin fail
with an instruction to replace it with `credentials-file`.

Plugin `profiles` publishes `NamedProfileService` over the same
`NamedProfileStore` used by CLI `--profile`. Listing and loading reject unsafe
names, symlinks/non-files, oversized or malformed documents, and embedded-name
mismatches. The service is effect-owned and refuses after Context shutdown.

K11 enforcement happens in `PluginFactories::build_profile`, after concrete
descriptors exist but before any plugin `apply`. A managed profile may restrict
implementation provenance to `built_in|unclassified` and deny any broad
descriptor capability. The same constraint section in user/project/local or
session authority fails at layer admission. The production loader uses this
single factory path, so a forbidden plugin cannot register a service, process,
tool or disposer before rejection.

Profile v3 may additionally carry one `[code_authority]` generation. Every
package row binds the fingerprint of the current PL08 source/catalog/host/policy
generation, exact id/version/tree digest, manifest runtime and entrypoint,
explicit grants, and the only admitted session policy:
`unique_per_activation`. WASI rows carry absolute host/guest preopen mappings
and literal IP+port endpoints; native rows cannot carry WASI resources.
Duplicate/widening rows, hostname authority, write-only preopens and
grant/resource disagreement fail during profile parsing.

The authority table is accepted only when trusted discovery binds the whole
document to `PluginScope::Managed` plus `ProfileSource::Managed`. User, named,
project, local-project and session layers cannot self-label or self-grant it.
The effective tree retains the typed generation for the composition root; it
contains configuration authority only and performs no filesystem or network
I/O.

Concrete scoped plugins are then stably topologically ordered by their exact
service `provides`/`injects`. A provider introduced by a higher profile layer
may move before an earlier base Consumer; unrelated rows keep their original
order and every row keeps its winning scope. Missing providers still fail at
composition, duplicate ownership still fails loud, and dependency cycles fail
at config construction naming the involved plugin ids.

## Layering, warnings and the effective report

Loading is a pure function of already-discovered paths (`Config::load_paths`
with `ConfigPaths { explicit, project, home }`; startup discovery fills them
in). An explicit `--config` file is the whole authority. Otherwise
`$HEYCODE_HOME/config.toml` is the base and a trusted project's `./heycode.toml` is
layered over it key by key — tables merge, scalars and arrays replace — so a
project file that sets only `[llm].model` keeps the home `[approval]`. The
source is reported as `ConfigSource::Layered { home, project }`. A project
overlay is a partial layer, never a migration subject; only the home file is
migrated automatically and an explicit file reports a pending migration.

Unknown keys in any file are `ConfigWarning::UnknownKey { path, key }` rows on
`LoadedConfig.warnings` — printed to stderr by headless runs and repeated on
the TUI transcript — never a startup failure. A type error is
`ConfigError::Parse` naming the file that actually carries the key, the dotted
key and its line in that file (`invalid type … at \`tools.bash_timeout_ms\`
(line 2)`).

`Config::apply_patch` (the command line's `--set`, `--model`, `--provider`, …)
records the patched paths; `Config::is_patched` lets composition treat those
values as an ephemeral top layer (see `heycode-routing`). `Config::report()`
returns a `ConfigReport`: every effective leaf with `ConfigValueSource::
{Default, File(path), Flag}` and a `render()` used by `/config` and
`heycode config show`. MCP server environment values render as `•••`.

## Saved-version migration matrix

Q17 keeps checked-in saved documents for the historical unversioned form and
every explicit root schema v1 through v28. Each historical fixture produces one
preview, commits only after a byte-exact versioned backup, parses as v28, then
produces no second plan; replaying the original plan returns `AlreadyApplied`
without changing destination or backup bytes. The current fixture is a no-op,
and a v28 control is refused unchanged with upgrade guidance before partial
loading.

`ConfigMigrationPlan::downgrade_guidance` is directional: when the original
backup fits an older reader it names that exact backup and schema; otherwise it
says a separately retained compatible copy is required.
`Config::downgrade_guidance` never invents a reverse semantic migration for a current
document without a backup. New optional default plugin rows need no schema step:
the historical generated-profile migration already removes the frozen snapshot
and derives its restored rows from the live built-in profile supplied by the
composition root.

Focused verification:

```sh
cargo clippy -p heycode-config --all-targets -- -D warnings
cargo test -p heycode-config
```

## S13 — competitor metadata preview

`preview_competitor_config` accepts already-read Codex `config.toml`, Claude
Code strict `settings.json` / `.mcp.json`, and OpenCode JSON or JSONC. It does
not accept a path and performs no writes. The preview retains screened
provider/model/base-URL metadata, credential-free MCP transports and a small
set of typed settings mappings; unknown fields remain visible by path and value
kind only.

Credential-, environment-, header-, OAuth- and helper-shaped fields are
represented only as `ExcludedImportField`, which has no value member. Hook
metadata is excluded the same way. Project rows remain previewable but cannot
produce a detached `Config` candidate until the host supplies project trust;
stdio MCP rows additionally require explicit executable authority. Candidate
construction clones an existing typed `Config`, refuses MCP name collisions,
clears imported-route credential pointers, and never mutates either source or
destination storage. An enabled MCP row that carried environment, headers,
OAuth or an unsupported working directory stays preview-only until MCP14 or
another typed owner can represent what was excluded; it is never applied as a
silently incomplete server.

Primary formats: [Codex configuration reference](https://developers.openai.com/codex/config-reference/),
[Claude Code settings](https://code.claude.com/docs/en/settings),
[Claude Code MCP](https://code.claude.com/docs/en/mcp),
[OpenCode config](https://opencode.ai/docs/config/), and
[OpenCode MCP](https://opencode.ai/docs/mcp-servers/).

This crate exposes a pure boundary, not a plugin: source discovery, canonical
workspace trust, UI confirmation, Settings/MCP persistence and credential
reference creation belong to their existing product owners.
