# MCP and plugin system plan

## Goal

MCP and plugins must be user-visible product capabilities, not configuration fragments hidden behind a partial client. A user can add, authenticate, inspect, enable, disable, diagnose and remove an integration from the TUI or CLI. Every integration has an owner, lifecycle, permissions and provenance.

## MCP service definition

`McpRegistry` owns configured server definitions and live connection generations.

```rust
#[async_trait]
pub trait McpRegistry: Send + Sync {
    async fn add(&self, definition: McpServerDefinition) -> Result<McpServerId, McpError>;
    async fn update(&self, id: &McpServerId, patch: McpServerPatch) -> Result<(), McpError>;
    async fn remove(&self, id: &McpServerId) -> Result<(), McpError>;
    async fn enable(&self, id: &McpServerId, enabled: bool) -> Result<(), McpError>;
    async fn reconnect(&self, id: &McpServerId) -> Result<(), McpError>;
    async fn authenticate(&self, id: &McpServerId, ui: &dyn AuthorizationUi)
        -> Result<AuthOutcome, McpError>;
    fn snapshot(&self) -> Arc<McpSnapshot>;
}
```

The registry is a Service Definition. Stdio and Streamable HTTP connection providers implement transport behavior. Tool/resource/prompt consumers project capabilities into heycode.

Implemented MCP01/MCP02 publishes `McpRegistry` under service `"mcp"` through plugin `mcp-registry`, and the stdio bridge now injects/publishes beneath it. Definition Providers register immutable exact rows as effects; connection Providers receive token-guarded publishers for starting/auth/reconnect/failure and atomic successful generations. Schema-v1 snapshots are deterministic/redacted while trusted Consumers read private definitions with no Debug/serde. Candidate stdio tools register atomically with token-owned RAII handles; Ready commits only after the complete set exists, and shutdown kills transport before removing rows even from held registries. MCP10 still owns legitimate persistence/mutation UX; HTTP/OAuth/resources/prompts remain later tasks.

## Supported MCP features

### Transport

- Stdio child process with explicit command, args, cwd and environment references.
- Streamable HTTP.
- Legacy SSE import only as a migration adapter, not a new configuration default.
- Configurable startup, request, idle and shutdown budgets.
- Required/optional startup behavior.
- Reconnect with bounded exponential backoff and last-good generation retention.

### Protocol lifecycle

- Initialize, negotiated protocol version and server capabilities.
- Server instructions.
- Tools list/call and `tools/list_changed`.
- Resources list/read/subscribe and change notifications.
- Prompts list/get and argument schemas.
- Logging and progress notifications.
- Roots negotiation.
- Elicitation forms/URL flows routed to the active UI.
- Sampling requests only after a dedicated policy and Consumer exist; default deny.
- Cancellation and progress tokens.

### Authentication

- Bearer token by credential reference.
- Environment-backed headers by credential reference.
- OAuth 2.1 Authorization Code + PKCE.
- Dynamic Client Registration.
- Client ID Metadata Documents where interoperable.
- Pre-registered client id/secret.
- Fixed or ephemeral callback port and configurable callback URL.
- Refresh, expiry, logout and reauthentication-required state.

OAuth credentials are stored as provider-owned credential records outside project configuration. Static secrets never appear in MCP definitions rendered to the model or UI.

## Server definition

```text
id / display name
scope: user | project | local | managed | plugin
transport: stdio | streamable-http
command/args/cwd OR url
credential/header references
enabled / required
startup and tool timeouts
enabled-tools / disabled-tools
default and per-tool approval mode
reconnect policy
resource/prompt exposure policy
```

Project MCP definitions load only after workspace trust. Managed policy can require, forbid or pin server identities.

## Tool registration

Public names are deterministic `mcp__<server>__<tool>` identifiers. Normalization includes a stable hash when truncation or replacement could collide.

A tool generation registers atomically:

1. Fetch and validate the complete paginated list.
2. Validate raw names, schemas, annotations and output schemas.
3. Build the candidate public generation.
4. Reject collisions before publishing any candidate tool.
5. Swap generations and dispose the prior one.

A failed refresh leaves the last good generation active. An exhausted reconnect policy removes the generation and publishes a visible failure state.

Tool annotations inform approval but never bypass policy. Destructive or unknown tools default to prompt. Read-only annotations may reduce prompts only when server identity is trusted and policy allows it.

## MCP results

Preserve ordered content blocks and structured content in an execution-local result. Durable projection supports:

- Text.
- Resource links.
- Bounded resource text.
- Images admitted through the attachment service and model capability check.
- Explicit diagnostics for unsupported audio/embedded resources rather than silent loss.
- Structured content validated against supported output schema.
- `isError` mapped to tool failure before rich-content persistence.

Large content spills to retained output storage with a bounded preview and content address.

## MCP UX

### `/mcp`

The list shows server, scope, transport, status, authentication, contribution counts, required flag and last error.

Actions:

- Add local server.
- Add remote server.
- Import preview from Codex, Claude Code or OpenCode.
- Authenticate/logout.
- Enable/disable.
- Reconnect.
- Inspect instructions/tools/resources/prompts.
- Test one tool with schema-generated form.
- Edit policy/timeouts.
- View bounded logs.
- Remove.

`heycode mcp` exposes equivalent non-interactive subcommands with JSON output.

### Import

Import reads only non-secret server metadata. Environment/header values become unresolved credential references. Existing OAuth grants remain with the source product; heycode runs its own OAuth flow.

The preview reports unsupported transport/features and exact config changes before writing.

## Plugin definition

A plugin is a versioned package of contributions. Initial stable plugin contents are declarative or process/MCP-backed:

```text
plugin/
  .heycode-plugin/plugin.toml
  skills/
  commands/
  agents/
  hooks/
  themes/
  providers/
  mcp/
  assets/
  wasm/                 # future, versioned WASI components
```

Manifest fields:

- Stable id, name, version, description and license.
- Minimum/maximum heycode API version.
- Contribution paths.
- Configuration schema.
- Requested capabilities and default enablement.
- Platform support.
- Source, checksum/signature and update channel.
- Dependencies and conflicts.
- Authentication policy.

Plugin ids are namespaced by marketplace. Contribution names are plugin-namespaced unless a registry explicitly supports a declared override.

Implemented PL01/PL02 provides `heycode-extensions`, closed `ManifestValidator` schema v1 and the versioned local cache. Admission validates namespaced ids/contributions, API/platform compatibility, SemVer, portable paths, permissions/references, source/checksum/canonical Ed25519 metadata, dependencies/conflicts and atomic public-name collisions. Unix install revalidates package bytes through held descriptor-relative traversal, rejects symlink/hardlink/special/case-collision/race surfaces, stores canonical SHA-256 content objects and publishes immutable id/version refs with OS-backed leases plus no-clobber commit. Errors are path/body-free. PL05 adds pinned marketplace catalogs and provenance: a catalog is verified against an operator-configured digest before it is parsed, an install is admitted only against the id/version the caller *requested*, and origin requires host-held evidence so a package's own claim can never establish it. PL03 still owns activation; **remote fetch and publisher signature verification remain unowned** — a signature is recorded, never verified. Non-Unix cache security remains unsupported.

## Plugin contributions

### Skills

`SKILL.md` with references/scripts/assets. Names/descriptions are indexed; full body loads on demand. User-only skills are invisible to the model until explicit invocation.

### Commands

Commands may:

- Open a UI action.
- Run a safe executable protocol.
- Invoke an MCP tool.
- Load a skill/prompt workflow.

Commands declare args, availability, execution timing, side effects and output renderer.

### Agents

Agent presets declare runtime/provider/model, instructions, tools, skills, permissions and max depth. They cannot embed credentials.

### Hooks

Typed event, matcher, handler type, timeout and failure policy. Handler types: command, HTTP, MCP tool, prompt or subagent. Project/plugin hooks require trust.

### Provider declarations

Declarative routes can compose a supported protocol adapter with endpoint, catalog source, auth reference and compatibility settings. They cannot claim unverified native capabilities; custom capability overrides are visibly user-authored and conservative.

### MCP bundle

Plugin-provided server definitions use paths relative to the installed plugin and cannot be rewritten by arbitrary environment interpolation. User policy can disable servers/tools without editing package files.

### Theme/UI metadata

Themes use semantic tokens. Stable plugin v1 does not load arbitrary native UI code. A future WASI UI description may contribute declarative components, never terminal backend access.

## Marketplace

A marketplace is a signed/versioned catalog fetched from:

- Local directory.
- Git repository/ref.
- HTTPS catalog.
- Supported package registry.

Marketplace configuration is distinct from plugin source. A catalog may reference plugins from Git, archives, package registries or local paths.

Install flow:

1. Fetch catalog/plugin metadata.
2. Verify checksum/signature and source policy.
3. Resolve dependency graph and platform compatibility.
4. Render exact capabilities, executable code, MCP endpoints and auth needs.
5. Ask for trust at action time.
6. Install into a content-addressed versioned cache.
7. Validate manifest and configuration schemas.
8. Activate transactionally.
9. Retain prior version until health checks pass.

Updates show permission and dependency changes. Rollback is one action.

## External code boundary

### Phase A — no arbitrary native code

Skills, commands, hooks, provider profiles and MCP cover the majority of extensions. Executable commands and MCP processes are explicit external processes governed by sandbox and policy.

### Phase B — process plugin protocol

A plugin process may register capabilities over a versioned JSON-RPC protocol. It receives only declared host capabilities. Process crash removes contributions and restarts according to policy.

### Phase C — WASI Component Model

WASI components provide a stable ABI, capability-based imports, memory isolation and portable packaging. WIT interfaces version independently. Filesystem/network/process access is absent unless granted.

Unsafe Rust dylibs remain out of scope because Rust has no stable ABI and an in-process native plugin can violate every lifecycle/security guarantee.

## Plugin lifecycle

States:

```text
discovered → installed → resolving → activating → active
                           └────────→ failed
active → updating → active(new) | active(old)+failed update
active → disabling → disabled
active/disabled → uninstalling → removed
```

Activation is a transaction. Contributions publish only after all declared services and configuration validate. Deactivation waits for active calls or cancels them according to the contribution contract, then disposes in reverse order.

## Plugin inspector

`/plugins verbose` and `heycode plugin inspect --json` show:

- Effective plugin tree and scopes.
- Service providers and consumers.
- Contribution inventory.
- Dependency and activation order.
- Configuration sources with secrets redacted.
- Active processes/MCP servers.
- Requested/granted capabilities.
- Last activation/update error.

A future model-facing `plugin_inspect` tool may expose a curated read-only view. Process-local self-modification follows only after capability isolation and explicit permission comparable to shell access.

## Security model

Threats:

- Malicious marketplace/catalog.
- Dependency substitution.
- Plugin install scripts.
- MCP server prompt injection and false annotations.
- OAuth redirect manipulation.
- Secret leakage through config, child environment or logs.
- Tool-schema context exhaustion.
- Plugin process escape or persistence after disable.

Controls:

- Trust and provenance UI.
- Pin versions, refs and checksums.
- No implicit install scripts.
- Capability grants and sandboxed processes.
- Credential references only.
- Domain/redirect validation and PKCE.
- Tool allowlists, deferred schemas and context budgets.
- Quiescent disposal tests.
- Managed allow/deny policies.

## MCP acceptance checklist

- [x] Definitions and token-owned connection generations have a deterministic redacted registry snapshot and last-good state model.
- [ ] Stdio and Streamable HTTP pass the official MCP inspector suite.
- [ ] OAuth PKCE, refresh, logout, DCR and pre-registered clients pass mock and live tests.
- [ ] Tools, resources, prompts and server instructions are discoverable in `/mcp`.
- [ ] List-change refresh swaps atomically.
- [ ] Crash-loop budget removes stale tools and reports status.
- [ ] Required-server failure blocks the profile with an actionable error.
- [ ] Optional-server failure never blocks the composer.
- [ ] Elicitation routes to the correct session/UI and cancels on turn end.
- [ ] Rich results preserve order and never leak unsupported payloads.
- [ ] Process exit and plugin unload reap every child.

## Plugin acceptance checklist

- [x] Manifest schema v1 rejects incompatible, non-portable, secret-bearing and colliding metadata before installation.
- [ ] Local plugin validate/install/enable/disable/update/rollback/remove workflows exist.
- [ ] Marketplace source and plugin source are independently pinned.
- [ ] Activation failure leaves no partial contribution.
- [ ] Every contribution disappears after disable/uninstall.
- [ ] Project plugins do not execute before workspace trust.
- [ ] Secret fields are redacted from every wire and diagnostic surface.
- [ ] Process plugin and WASI APIs have version negotiation and compatibility tests before public release.
