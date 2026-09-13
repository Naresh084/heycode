//! heycode-config — configuration discovery, parsing, and patching.
//!
//! Layering, lowest first: built-in defaults → `~/.heycode/config.toml` → a
//! trusted project's `./heycode.toml` (key by key: tables merge, scalars and
//! arrays replace) → `--set dotted.key=value` and the routing flags. An
//! explicit `--config <path>` replaces both files. Unknown keys in any file are
//! warnings ([`ConfigWarning`]); `--set` still fails loudly on unknown paths.
//! Unknown PLUGIN names fail at composition with the list of available
//! factories.

use serde::{Deserialize, Serialize};

mod competitor_import;
/// Immutable, explicitly confirmed configuration-import generations.
pub mod imports;
mod layering;
mod managed_code;
mod migration;
mod migration_doctor;
mod profile_store;
mod profiles;
mod scopes;

pub use competitor_import::{
    CompetitorConfigKind, CompetitorImportError, CompetitorImportPreview, ExcludedImportField,
    ImportAuthority, ImportExclusionReason, ImportFieldNotice, ImportReadiness, ImportScope,
    ImportSettingTarget, ImportValueKind, ImportedMcpServer, ImportedMcpTransport,
    ImportedProviderRoute, ImportedSetting, preview_competitor_config,
};
pub use layering::{
    ConfigLayer, ConfigPaths, ConfigReport, ConfigRow, ConfigValueSource, ConfigWarning,
};
pub use managed_code::{
    ManagedCodeAuthorityConfig, ManagedCodeAuthorityPackage, ManagedCodeGrant, ManagedCodeRuntime,
    ManagedCodeSessionPolicy, ManagedWasiNetworkEndpoint, ManagedWasiPreopen,
    ManagedWasiPreopenAccess,
};
pub use migration::{
    CONFIG_SCHEMA_VERSION, ConfigDowngradeGuidance, ConfigMigrationChange,
    ConfigMigrationDisposition, ConfigMigrationNotice, ConfigMigrationPlan, ConfigSource,
    ConfigVersionState, LoadedConfig, MigrationApplyOutcome,
};
pub use migration_doctor::config_migration_doctor_plugin;
pub use profile_store::{
    NamedProfileService, NamedProfileStore, NamedProfileSummary, SERVICE_PROFILES,
    named_profiles_plugin,
};
pub use profiles::{
    EffectiveProfileLayer, EffectiveProfilePlugin, EffectiveProfileTree, ManagedPluginCapability,
    ManagedPluginSource, ManagedProfileConstraints, PROFILE_SCHEMA_VERSION, ProfileDecision,
    ProfileDocument, ProfileLayer, ProfilePluginRow, ProfileSource, SourcedPluginSelection,
    resolve_profile_tree,
};
pub use scopes::{
    EffectivePluginSelection, PluginDirective, PluginScopeLayer, resolve_scoped_plugins,
};

/// Provider selected when no persisted or CLI choice exists.
pub const DEFAULT_LLM_PROVIDER: &str = "deepseek";
/// Current model selected with the built-in default provider.
pub const DEFAULT_LLM_MODEL: &str = "deepseek-v4-flash";

/// `[profile]` section: which plugins compose, in order.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProfileSection {
    /// Ordered plugin factory names.
    #[serde(default)]
    pub plugins: Vec<String>,
}

/// `[llm]` section: routing + credentials pointer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmSection {
    /// Provider factory name (`deepseek`, `openrouter`, …).
    pub provider: String,
    /// Model id passed through to the provider.
    pub model: String,
    /// Optional base-url override (proxies, gateways).
    pub base_url: Option<String>,
    /// Optional API-key env var override.
    pub api_key_env: Option<String>,
    /// Exact provider wire dialect when more than one reviewed adapter exists.
    pub protocol: LlmProtocolCfg,
    /// Optional explicit provider-owned output-token default.
    pub max_output_tokens: Option<u64>,
}

impl Default for LlmSection {
    fn default() -> Self {
        Self {
            provider: DEFAULT_LLM_PROVIDER.to_owned(),
            model: DEFAULT_LLM_MODEL.to_owned(),
            base_url: None,
            api_key_env: None,
            protocol: LlmProtocolCfg::Auto,
            max_output_tokens: None,
        }
    }
}

/// Explicit inference wire selection at the configuration boundary.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmProtocolCfg {
    /// Use the selected provider's established product default.
    #[default]
    Auto,
    /// OpenAI-shaped Chat Completions.
    #[serde(rename = "openai_chat", alias = "open_ai_chat")]
    OpenAiChat,
    /// OpenAI Responses.
    #[serde(rename = "openai_responses", alias = "open_ai_responses")]
    OpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
}

impl std::fmt::Display for LlmProtocolCfg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
        })
    }
}

/// `[tools]` section: explicit caps (no hidden defaults inside tools).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ToolsSection {
    /// Register the E07 persistent-terminal tools. Off by default: a terminal
    /// holds a live process for the life of the session, so it is opt-in.
    #[serde(default)]
    pub terminals_enabled: bool,
    /// Bytes retained per execution stdout/stderr/terminal stream (1 KiB..=1 MiB).
    pub execution_retained_bytes: usize,
    /// Inline output body cap (1 KiB..=32 KiB); larger output is paged with job_output.
    pub execution_inline_bytes: usize,
    /// Number of execution output histories retained in this session (1..=256).
    pub execution_history_limit: usize,
    /// Seconds before an eligible foreground tool is promoted; zero disables it.
    pub foreground_timeout_secs: u64,
    /// Bash timeout in milliseconds.
    pub bash_timeout_ms: u64,
    /// Max bytes per `read`.
    pub read_max_bytes: usize,
    /// Max lines per `read`.
    pub read_max_lines: usize,
}

impl Default for ToolsSection {
    fn default() -> Self {
        Self {
            terminals_enabled: false,
            execution_retained_bytes: 256 * 1024,
            execution_inline_bytes: 6 * 1024,
            execution_history_limit: 64,
            foreground_timeout_secs: 120,
            bash_timeout_ms: 600_000,
            read_max_bytes: 262_144,
            read_max_lines: 2_000,
        }
    }
}

/// `[approval]` section: tool-call permission policy.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct ApprovalSection {
    /// `full_access` asks no permission; `accepted_edits` remembers each approved
    /// action for the conversation; `ask` (alias `default`) asks every time.
    /// `auto` requires AI classification and is unavailable without it. `deny`
    /// remains a non-menu compatibility control. Unset: interactive `ask`, headless `full_access`.
    #[serde(default)]
    pub mode: Option<ApprovalMode>,
}

impl ApprovalSection {
    /// The mode this surface runs with: the configured one, else `ask` for an
    /// interactive shell and `full_access` for a headless run.
    ///
    /// Claude Code and Codex both ask by default in the terminal; only an
    /// automation with an explicit flag runs tools unattended.
    #[must_use]
    pub fn effective(&self, interactive: bool) -> ApprovalMode {
        self.mode.unwrap_or(if interactive {
            ApprovalMode::Ask
        } else {
            ApprovalMode::FullAccess
        })
    }
}

/// Tool-call policy modes.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Approve all calls (headless default).
    #[default]
    #[serde(rename = "full_access", alias = "full")]
    FullAccess,
    /// Ask once per named action for this conversation.
    #[serde(rename = "accepted_edits", alias = "accepted")]
    AcceptedEdits,
    /// AI classification when supported by the active connection.
    Auto,
    /// Route every call through an interactive dialog (TUI).
    #[serde(alias = "default")]
    Ask,
    /// Refuse every tool call.
    Deny,
}

impl std::fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FullAccess => "full_access",
            Self::AcceptedEdits => "accepted_edits",
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Deny => "deny",
        })
    }
}

/// `[compaction]` section: automatic context folding.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CompactionSection {
    /// Fold automatically when projected context crosses the threshold.
    pub auto: bool,
    /// Fraction of [`Self::context_window`] that triggers folding.
    pub threshold_ratio: f32,
    /// Explicit context cap/fallback in tokens; zero uses the active model limit.
    pub context_window: u64,
}

impl Default for CompactionSection {
    fn default() -> Self {
        Self {
            auto: true,
            threshold_ratio: 0.8,
            context_window: 0,
        }
    }
}

/// `[subagent]` section: nested-task limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SubagentSection {
    /// Maximum nesting depth of `task` calls (1 = flat children only).
    pub max_depth: u32,
    /// Exact full Git commit used by optional isolated worktree providers.
    pub worktree_base: Option<String>,
    /// Shared native descendant inference concurrency; zero means unlimited.
    pub max_concurrent: usize,
    /// Session-lifetime inference dispatch limit; zero means unlimited.
    pub max_provider_requests: u64,
    /// Optional descendant output-token cap; zero uses provider/model defaults.
    pub max_output_tokens: u32,
}

impl Default for SubagentSection {
    fn default() -> Self {
        Self {
            max_depth: 3,
            worktree_base: None,
            max_concurrent: 0,
            max_provider_requests: 0,
            max_output_tokens: 0,
        }
    }
}

/// `[jobs]` admission and durable retention settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct JobsSection {
    /// Concurrent process operations; coordinators use separately bounded inference.
    pub max_concurrent: usize,
    /// Waiting/coordination row ceiling beyond the running capacity.
    pub max_queued: usize,
    /// Retained settled summaries (active handles are never silently pruned).
    pub max_history: usize,
    /// Session-lifetime job admission cap; zero means unlimited.
    pub max_admissions: u64,
}
impl Default for JobsSection {
    fn default() -> Self {
        Self {
            max_concurrent: 8,
            max_queued: 64,
            max_history: 256,
            max_admissions: 0,
        }
    }
}

/// One configured MCP server.
///
/// Exactly one transport must be selected: `command` (stdio) or `url`
/// (Streamable HTTP). Naming both is ambiguous and naming neither leaves the
/// server unreachable, so both fail loud at load rather than silently choosing.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerCfg {
    /// Executable to spawn (resolved via PATH). Selects the stdio transport.
    #[serde(default)]
    pub command: Option<String>,
    /// Streamable HTTP endpoint. Selects the HTTP transport.
    #[serde(default)]
    pub url: Option<String>,
    /// Argument vector. Stdio only.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the child. Stdio only.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Whether a failure to connect this server fails the whole session.
    ///
    /// Off by default: a mistyped command or a binary that is not installed
    /// yet shows as a failed server in `/mcp` and heycode still starts. Set it
    /// when the session is meaningless without the server.
    #[serde(default)]
    pub required: bool,
}

/// The transport one configured MCP server selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerTransport {
    /// Spawn a local process and speak JSON-RPC over its stdio.
    Stdio {
        /// Executable resolved via PATH.
        command: String,
        /// Exact argument vector.
        args: Vec<String>,
        /// Extra environment layered onto the scrubbed parent environment.
        env: std::collections::HashMap<String, String>,
    },
    /// Speak Streamable HTTP to a remote endpoint.
    StreamableHttp {
        /// Absolute endpoint URL.
        url: String,
    },
}

impl McpServerCfg {
    /// Resolve the exact transport this configuration selected.
    ///
    /// # Errors
    /// Naming both `command` and `url`, naming neither, or pairing stdio-only
    /// `args`/`env` with an HTTP endpoint.
    pub fn transport(&self, name: &str) -> Result<McpServerTransport, ConfigError> {
        match (self.command.as_deref(), self.url.as_deref()) {
            (Some(_), Some(_)) => Err(ConfigError::Invalid(format!(
                "[mcp.servers.{name}] sets both `command` and `url`; choose exactly one transport"
            ))),
            (None, None) => Err(ConfigError::Invalid(format!(
                "[mcp.servers.{name}] sets neither `command` nor `url`; choose exactly one transport"
            ))),
            (Some(command), None) => Ok(McpServerTransport::Stdio {
                command: command.to_owned(),
                args: self.args.clone(),
                env: self.env.clone(),
            }),
            (None, Some(url)) => {
                if !self.args.is_empty() || !self.env.is_empty() {
                    return Err(ConfigError::Invalid(format!(
                        "[mcp.servers.{name}] sets `args`/`env` with `url`; those apply only to a stdio `command`"
                    )));
                }
                Ok(McpServerTransport::StreamableHttp {
                    url: url.to_owned(),
                })
            }
        }
    }
}

/// `[mcp]` section: external tool servers (the plugin mechanism).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpSection {
    /// Server name → launch configuration; tools register as
    /// `mcp__<server>__<tool>`.
    #[serde(default)]
    pub servers: std::collections::HashMap<String, McpServerCfg>,
}

/// `[web]` section: model-facing web tools.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WebSection {
    /// Register `web_fetch` / `web_search`. Search works keyless via
    /// DuckDuckGo Lite; set `BRAVE_API_KEY` to upgrade result quality.
    pub enabled: bool,
}

impl Default for WebSection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[sandbox]` section: bash confinement.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct SandboxSection {
    /// `off` (default) | `readonly` | `workspace`.
    #[serde(default)]
    pub mode: SandboxModeCfg,
}

/// Bash confinement levels.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SandboxModeCfg {
    /// No confinement.
    #[default]
    Off,
    /// Reads everywhere, writes nowhere.
    ReadOnly,
    /// Writes confined under the session workspace.
    Workspace,
}

impl std::fmt::Display for SandboxModeCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::ReadOnly => "readonly",
            Self::Workspace => "workspace",
        })
    }
}

/// `[ui]` section: presentation knobs.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UiSection {
    /// Accent color as `#RRGGBB`; defaults to the Claude-orange token.
    #[serde(default)]
    pub accent: Option<String>,
    /// Generate a session title after the first turn (opt-in; costs one
    /// provider call).
    #[serde(default)]
    pub auto_title: bool,
}

/// The whole parsed configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Persisted document schema. Missing means the legacy unversioned format
    /// and is interpreted as the current shape until the migration is applied.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    /// Plugin composition.
    #[serde(default)]
    pub profile: ProfileSection,
    /// Provider routing.
    #[serde(default)]
    pub llm: LlmSection,
    /// Tool caps.
    #[serde(default)]
    pub tools: ToolsSection,
    /// Presentation.
    #[serde(default)]
    pub ui: UiSection,
    /// Tool-call policy.
    #[serde(default)]
    pub approval: ApprovalSection,
    /// Automatic compaction policy.
    #[serde(default)]
    pub compaction: CompactionSection,
    /// Nested task limits.
    #[serde(default)]
    pub subagent: SubagentSection,
    /// Global job admission and bounded history.
    #[serde(default)]
    pub jobs: JobsSection,
    /// Bash confinement.
    #[serde(default)]
    pub sandbox: SandboxSection,
    /// Web tools.
    #[serde(default)]
    pub web: WebSection,
    /// External MCP tool servers.
    #[serde(default)]
    pub mcp: McpSection,
    /// Dotted paths successfully changed by [`Config::apply_patch`] — the
    /// command line's `--set`, `--model`, `--provider`, … . Never persisted:
    /// a flag is an ephemeral layer for this process, not a config edit.
    #[serde(skip)]
    patched: std::collections::BTreeSet<String>,
    /// Files this configuration was loaded from, lowest layer first, so
    /// [`Config::report`] can say where each value came from.
    #[serde(skip)]
    layers: Vec<ConfigLayer>,
    /// In-process provenance of admitted settings applied after config files.
    #[serde(skip)]
    effective_sources: std::collections::BTreeMap<String, ConfigValueSource>,
}

/// Configuration failures surfaced to the user.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// File could not be read.
    #[error("failed to read config {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// TOML did not match the schema.
    #[error("invalid config in {path}: {message}")]
    Parse {
        /// Offending path.
        path: String,
        /// Human-readable parse error.
        message: String,
    },
    /// The file was written by a newer heycode schema than this binary knows.
    #[error(
        "{path} uses newer config schema {found}; this heycode supports schema {supported} — upgrade heycode before loading it"
    )]
    NewerSchema {
        /// Offending path or in-memory document label.
        path: String,
        /// Version declared by the document.
        found: u32,
        /// Highest version supported by this binary.
        supported: u32,
    },
    /// A migration refused to overwrite state that changed since its preview.
    #[error("cannot migrate config {path}: {message}")]
    MigrationConflict {
        /// Config or backup path involved.
        path: String,
        /// Actionable conflict explanation.
        message: String,
    },
    /// A `--set` patch named an unknown path.
    #[error(
        "unknown config path `{0}` — valid: llm.provider, llm.model, llm.base_url, llm.api_key_env, llm.protocol, llm.max_output_tokens, ui.accent, tools.bash_timeout_ms, tools.read_max_bytes, tools.read_max_lines"
    )]
    UnknownPatchPath(String),
    /// A `--set` value was malformed (`key=value` split or bad type).
    #[error("invalid patch `{0}` — expected `dotted.key=value`")]
    MalformedPatch(String),
    /// A configured section violated a rule serde alone cannot express.
    #[error("{0}")]
    Invalid(String),
    /// A patch value failed to convert to the target field type.
    #[error("patch `{path}` value `{value}` is not valid: {message}")]
    BadPatchValue {
        /// Patched path.
        path: String,
        /// Raw value text.
        value: String,
        /// Conversion failure reason.
        message: String,
    },
}

impl Config {
    /// Built-in defaults used when no file exists.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            // Empty means "the composition root's built-in profile". Naming a
            // list here would fork the default order into a second source of
            // truth that silently goes stale as plugins are added.
            profile: ProfileSection::default(),
            llm: LlmSection::default(),
            tools: ToolsSection::default(),
            ui: UiSection::default(),
            approval: ApprovalSection::default(),
            compaction: CompactionSection::default(),
            subagent: SubagentSection::default(),
            jobs: JobsSection::default(),
            sandbox: SandboxSection::default(),
            web: WebSection::default(),
            mcp: McpSection::default(),
            patched: std::collections::BTreeSet::new(),
            layers: Vec::new(),
            effective_sources: std::collections::BTreeMap::new(),
        }
    }

    /// Generate truthful guidance for opening this document with an older
    /// configuration reader.
    ///
    /// This value has no associated migration backup, so an incompatible
    /// reader requires a separately retained compatible copy. heycode does not
    /// claim it can reverse semantic migrations.
    #[must_use]
    pub const fn downgrade_guidance(&self, reader_max_schema: u32) -> ConfigDowngradeGuidance {
        if self.schema_version <= reader_max_schema {
            ConfigDowngradeGuidance::Compatible {
                document_schema: self.schema_version,
                reader_max_schema,
            }
        } else {
            ConfigDowngradeGuidance::RequiresCompatibleCopy {
                document_schema: self.schema_version,
                reader_max_schema,
            }
        }
    }

    /// Load config through the same layering as startup, without migration
    /// side effects being reported: the explicit path when given, else home
    /// with a trusted-or-not `./heycode.toml` layered over it.
    ///
    /// # Errors
    /// [`ConfigError::Io`] / [`ConfigError::Parse`] when an existing file
    /// cannot be honored (a broken explicit/project config fails loud; only
    /// the home fallback may be absent).
    pub fn load(explicit: Option<&std::path::Path>) -> Result<Self, ConfigError> {
        let project = project_config_path(std::path::Path::new("."));
        Self::load_paths(
            ConfigPaths {
                explicit: explicit.map(std::path::Path::to_path_buf),
                project: project.is_file().then_some(project),
                home: dirs_home()?.map(|home| home.join("config.toml")),
            },
            &[],
        )
        .map(|loaded| loaded.config)
    }

    /// Load the winning config source and apply only migrations proven safe for
    /// the old setup-generated HOME document.
    ///
    /// Explicit and project files remain user-owned: their migration is
    /// returned as pending and their bytes are never changed automatically.
    ///
    /// # Errors
    /// Discovery I/O, parse/version errors, backup conflicts, or atomic-write
    /// failures abort startup before composition.
    pub fn load_for_startup(
        explicit: Option<&std::path::Path>,
        current_builtin_profile: &[&str],
    ) -> Result<LoadedConfig, ConfigError> {
        Self::load_for_startup_with_project(explicit, current_builtin_profile, true)
    }

    /// Load startup configuration with an explicit automatic-project discovery gate.
    ///
    /// An explicit `--config` path remains explicit authority. When
    /// `allow_project` is false, only automatic `./heycode.toml` discovery is
    /// skipped; home/default fallback remains unchanged. The composition root
    /// must derive this flag from a pre-opened workspace-trust service.
    ///
    /// # Errors
    /// Same as [`Self::load_for_startup`].
    pub fn load_for_startup_with_project(
        explicit: Option<&std::path::Path>,
        current_builtin_profile: &[&str],
        allow_project: bool,
    ) -> Result<LoadedConfig, ConfigError> {
        let project = project_config_path(std::path::Path::new("."));
        Self::load_paths(
            ConfigPaths {
                explicit: explicit.map(std::path::Path::to_path_buf),
                project: (allow_project && project.is_file()).then_some(project),
                home: dirs_home()?.map(|home| home.join("config.toml")),
            },
            current_builtin_profile,
        )
    }

    /// Parse one TOML file into a full config (sections default-fill).
    ///
    /// # Errors
    /// See [`Config::load`].
    pub fn from_file(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if let ConfigVersionState::Newer(found) =
            migration::classify_document(&raw, &path.display().to_string())?
        {
            return Err(ConfigError::NewerSchema {
                path: path.display().to_string(),
                found,
                supported: CONFIG_SCHEMA_VERSION,
            });
        }
        let mut table: toml::Table =
            toml::from_str(&raw).map_err(|e: toml::de::Error| ConfigError::Parse {
                path: path.display().to_string(),
                message: e.message().to_owned(),
            })?;
        migration::normalize_legacy_approval(&mut table);
        table
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Parse {
                path: path.display().to_string(),
                message: e.message().to_owned(),
            })
    }

    /// Classify an in-memory TOML document by schema version.
    ///
    /// # Errors
    /// Malformed TOML or a non-integer/out-of-range `schema_version` fails
    /// loudly as [`ConfigError::Parse`].
    pub fn classify_document(raw: &str) -> Result<ConfigVersionState, ConfigError> {
        migration::classify_document(raw, "<document>")
    }

    /// Apply one `--set dotted.key=value` patch in place.
    ///
    /// # Errors
    /// [`ConfigError`] variants for malformed input or unknown paths.
    pub fn apply_patch(&mut self, patch: &str) -> Result<(), ConfigError> {
        let (path, value) = patch
            .split_once('=')
            .ok_or_else(|| ConfigError::MalformedPatch(patch.to_owned()))?;
        let value_text = value.to_owned();
        macro_rules! set_u64 {
            ($field:expr) => {{
                $field = value_text
                    .parse::<u64>()
                    .map_err(|e| ConfigError::BadPatchValue {
                        path: path.to_owned(),
                        value: value_text.clone(),
                        message: e.to_string(),
                    })?;
            }};
        }
        match path {
            "llm.provider" => self.llm.provider = value_text,
            "llm.model" => self.llm.model = value_text,
            "llm.base_url" => self.llm.base_url = Some(value_text),
            "llm.api_key_env" => self.llm.api_key_env = Some(value_text),
            "llm.protocol" => {
                self.llm.protocol = match value_text.as_str() {
                    "auto" => LlmProtocolCfg::Auto,
                    "openai_chat" => LlmProtocolCfg::OpenAiChat,
                    "openai_responses" => LlmProtocolCfg::OpenAiResponses,
                    "anthropic_messages" => LlmProtocolCfg::AnthropicMessages,
                    _ => {
                        return Err(ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text,
                            message: "expected auto, openai_chat, openai_responses, or anthropic_messages".to_owned(),
                        });
                    }
                };
            }
            "llm.max_output_tokens" => {
                let parsed =
                    value_text
                        .parse::<u64>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
                if parsed == 0 {
                    return Err(ConfigError::BadPatchValue {
                        path: path.to_owned(),
                        value: value_text,
                        message: "expected a positive token count".to_owned(),
                    });
                }
                self.llm.max_output_tokens = Some(parsed);
            }
            "ui.accent" => self.ui.accent = Some(value_text),
            "ui.auto_title" => {
                self.ui.auto_title = parse_bool(path, &value_text)?;
            }
            "compaction.auto" => {
                self.compaction.auto = parse_bool(path, &value_text)?;
            }
            "compaction.threshold_ratio" => {
                self.compaction.threshold_ratio =
                    value_text
                        .parse::<f32>()
                        .map_err(|e| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: e.to_string(),
                        })?;
            }
            "compaction.context_window" => set_u64!(self.compaction.context_window),
            "sandbox.mode" => {
                self.sandbox.mode = match value_text.as_str() {
                    "off" => SandboxModeCfg::Off,
                    "readonly" => SandboxModeCfg::ReadOnly,
                    "workspace" => SandboxModeCfg::Workspace,
                    other => {
                        return Err(ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: format!("expected off|readonly|workspace, got `{other}`"),
                        });
                    }
                };
            }
            "subagent.max_concurrent" => {
                self.subagent.max_concurrent =
                    value_text
                        .parse::<usize>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "subagent.max_provider_requests" => {
                self.subagent.max_provider_requests =
                    value_text
                        .parse::<u64>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "subagent.max_output_tokens" => {
                self.subagent.max_output_tokens =
                    value_text
                        .parse::<u32>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "jobs.max_concurrent" => {
                self.jobs.max_concurrent =
                    value_text
                        .parse::<usize>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "jobs.max_queued" => {
                self.jobs.max_queued =
                    value_text
                        .parse::<usize>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "jobs.max_history" => {
                self.jobs.max_history =
                    value_text
                        .parse::<usize>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "jobs.max_admissions" => {
                self.jobs.max_admissions =
                    value_text
                        .parse::<u64>()
                        .map_err(|error| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: error.to_string(),
                        })?;
            }
            "subagent.max_depth" => {
                self.subagent.max_depth =
                    value_text
                        .parse::<u32>()
                        .map_err(|e| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: e.to_string(),
                        })?;
            }
            "subagent.worktree_base" => {
                self.subagent.worktree_base = Some(value_text);
            }
            "approval.mode" => {
                self.approval.mode = Some(match value_text.as_str() {
                    "full_access" | "full" => ApprovalMode::FullAccess,
                    "accepted_edits" | "accepted" => ApprovalMode::AcceptedEdits,
                    "auto" => ApprovalMode::Auto,
                    "ask" | "default" => ApprovalMode::Ask,
                    "deny" => ApprovalMode::Deny,
                    other => {
                        return Err(ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: format!(
                                "expected full_access|accepted_edits|default|auto|deny, got `{other}`"
                            ),
                        });
                    }
                });
            }
            "tools.bash_timeout_ms" => set_u64!(self.tools.bash_timeout_ms),
            "tools.read_max_bytes" => {
                self.tools.read_max_bytes =
                    value_text
                        .parse::<usize>()
                        .map_err(|e| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: e.to_string(),
                        })?;
            }
            "tools.read_max_lines" => {
                self.tools.read_max_lines =
                    value_text
                        .parse::<usize>()
                        .map_err(|e| ConfigError::BadPatchValue {
                            path: path.to_owned(),
                            value: value_text.clone(),
                            message: e.to_string(),
                        })?;
            }
            other => return Err(ConfigError::UnknownPatchPath(other.to_owned())),
        }
        self.patched.insert(path.to_owned());
        Ok(())
    }

    /// Dotted paths the command line patched, in path order.
    pub fn patched_paths(&self) -> impl Iterator<Item = &str> {
        self.patched.iter().map(String::as_str)
    }

    /// Whether the command line patched `path` (e.g. `llm.model`).
    #[must_use]
    pub fn is_patched(&self, path: &str) -> bool {
        self.patched.contains(path)
    }

    /// Attribute an effective value to the admitted settings that supplied it.
    /// Explicit command-line patches always retain precedence in the report.
    pub fn set_effective_source(&mut self, path: &str, source: ConfigValueSource) {
        self.effective_sources.insert(path.to_owned(), source);
    }

    /// Files this configuration was loaded from, lowest layer first.
    #[must_use]
    pub fn layers(&self) -> &[ConfigLayer] {
        &self.layers
    }

    /// Every effective value with the layer or flag that supplied it.
    #[must_use]
    pub fn report(&self) -> ConfigReport {
        ConfigReport::new(self, &self.layers)
    }

    /// Validate plugin names against the installed factory list, returning
    /// the composed order.
    ///
    /// # Errors
    /// Names the first unknown plugin and lists what IS available.
    pub fn resolve_plugins(&self, available: &[&str]) -> Result<Vec<String>, ConfigError> {
        for name in &self.profile.plugins {
            if !available.contains(&name.as_str()) {
                return Err(ConfigError::Parse {
                    path: "<composition>".to_owned(),
                    message: format!(
                        "{} — available: {}",
                        unknown_plugin_reason(name),
                        available.join(", ")
                    ),
                });
            }
        }
        Ok(self.profile.plugins.clone())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

const fn current_schema_version() -> u32 {
    CONFIG_SCHEMA_VERSION
}

/// Parse a `--set` boolean strictly.
///
/// # Errors
/// [`ConfigError::BadPatchValue`] naming the path and the rejected value —
/// principle #4: malformed config fails loud rather than defaulting to false.
fn parse_bool(path: &str, value: &str) -> Result<bool, ConfigError> {
    match value {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        other => Err(ConfigError::BadPatchValue {
            path: path.to_owned(),
            value: other.to_owned(),
            message: "expected a boolean: true/false/yes/no/1/0/on/off".to_owned(),
        }),
    }
}

/// The plugin factory registry: plugin name → the constructor that builds it.
///
/// heycode-config owns the TYPE; the composition root owns the TABLE, because
/// only the bin may know every crate (AGENTS.md §2 — libraries never reach up).
/// Constructors are `FnOnce` so a factory may move captured configuration,
/// providers, or a sessions directory into the plugin it builds.
#[derive(Default)]
pub struct PluginFactories {
    map: std::collections::BTreeMap<String, Box<dyn FnOnce() -> Box<dyn heycode_core::Plugin>>>,
}

impl PluginFactories {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one factory. A duplicate name replaces the earlier entry —
    /// composition order comes from the profile, not from this map.
    pub fn register(
        &mut self,
        name: &str,
        make: impl FnOnce() -> Box<dyn heycode_core::Plugin> + 'static,
    ) {
        self.map.insert(name.to_owned(), Box::new(make));
    }

    /// Registered names, sorted — the "available" list in failure messages.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// Build the plugin vector for `order`, consuming the registry.
    ///
    /// # Errors
    /// [`ConfigError::Parse`] naming the first unknown plugin plus every
    /// available name (principle #4: fail loud at load, never skip-and-continue),
    /// or naming a plugin listed twice.
    pub fn build(
        mut self,
        order: &[String],
    ) -> Result<Vec<Box<dyn heycode_core::Plugin>>, ConfigError> {
        let available = self.names().join(", ");
        let mut out = Vec::with_capacity(order.len());
        for name in order {
            let Some(make) = self.map.remove(name) else {
                return Err(ConfigError::Parse {
                    path: "<composition>".to_owned(),
                    message: format!("{} — available: {available}", unknown_plugin_reason(name)),
                });
            };
            out.push(make());
        }
        Ok(out)
    }

    /// Build concrete scoped plugin instances from one resolved effective
    /// selection, consuming each factory exactly once.
    ///
    /// # Errors
    /// Unknown or duplicate effective ids, with the same fail-loud available
    /// list as [`Self::build`].
    pub fn build_scoped(
        mut self,
        selection: &[EffectivePluginSelection],
    ) -> Result<Vec<heycode_core::ScopedPlugin>, ConfigError> {
        let available = self.names().join(", ");
        let mut out = Vec::with_capacity(selection.len());
        for row in selection {
            let Some(make) = self.map.remove(&row.id) else {
                return Err(ConfigError::Parse {
                    path: "<composition>".to_owned(),
                    message: format!(
                        "{} — available: {available}",
                        unknown_plugin_reason(&row.id)
                    ),
                });
            };
            out.push(heycode_core::ScopedPlugin::new(row.scope, make()));
        }
        stable_dependency_order(out)
    }

    /// Build one resolved source-aware profile and enforce its final managed
    /// admission policy before returning any activatable plugin.
    ///
    /// # Errors
    /// Unknown/repeated factories or any implementation source/contribution
    /// family forbidden by the profile's managed constraints.
    pub fn build_profile(
        self,
        tree: &EffectiveProfileTree,
    ) -> Result<Vec<heycode_core::ScopedPlugin>, ConfigError> {
        let selection = tree
            .enabled
            .iter()
            .map(|row| EffectivePluginSelection {
                id: row.id.clone(),
                scope: row.scope,
            })
            .collect::<Vec<_>>();
        let plugins = self.build_scoped(&selection)?;
        tree.admit_plugins(&plugins)?;
        Ok(plugins)
    }
}

fn unknown_plugin_reason(name: &str) -> String {
    if name == "credentials-keychain" {
        "plugin `credentials-keychain` is retired; replace it with `credentials-file` in this profile; credentials are stored only in the heycode home and OS entries are never imported".to_owned()
    } else {
        format!("unknown plugin `{name}`")
    }
}

fn stable_dependency_order(
    plugins: Vec<heycode_core::ScopedPlugin>,
) -> Result<Vec<heycode_core::ScopedPlugin>, ConfigError> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut providers = BTreeMap::<heycode_core::ServiceKey, usize>::new();
    for (index, scoped) in plugins.iter().enumerate() {
        for service in scoped.plugin().provides() {
            providers.entry(*service).or_insert(index);
        }
    }
    let mut outgoing = vec![BTreeSet::<usize>::new(); plugins.len()];
    let mut indegree = vec![0_usize; plugins.len()];
    for (consumer, scoped) in plugins.iter().enumerate() {
        for service in scoped.plugin().inject() {
            let Some(provider) = providers.get(service).copied() else {
                continue;
            };
            if provider != consumer && outgoing[provider].insert(consumer) {
                indegree[consumer] = indegree[consumer].saturating_add(1);
            }
        }
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(plugins.len());
    while let Some(index) = ready.pop_first() {
        order.push(index);
        for consumer in &outgoing[index] {
            indegree[*consumer] = indegree[*consumer].saturating_sub(1);
            if indegree[*consumer] == 0 {
                ready.insert(*consumer);
            }
        }
    }
    if order.len() != plugins.len() {
        let cycle = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, count)| (*count > 0).then_some(plugins[index].plugin().name()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ConfigError::Parse {
            path: "<composition>".to_owned(),
            message: format!("plugin service dependency cycle: {cycle}"),
        });
    }
    let mut owned = plugins.into_iter().map(Some).collect::<Vec<_>>();
    let mut sorted = Vec::with_capacity(owned.len());
    for index in order {
        let Some(plugin) = owned[index].take() else {
            return Err(ConfigError::Parse {
                path: "<composition>".to_owned(),
                message: "plugin dependency ordering repeated one row".to_owned(),
            });
        };
        sorted.push(plugin);
    }
    Ok(sorted)
}

fn dirs_home() -> Result<Option<std::path::PathBuf>, ConfigError> {
    home_root()
}

/// Resolve the shared state root using HEYCODE_HOME or ~/.heycode.
///
/// # Errors
/// Relative environment overrides are rejected.
pub fn home_root() -> Result<Option<std::path::PathBuf>, ConfigError> {
    resolve_home_root(std::env::var_os("HEYCODE_HOME"), dirs::home_dir())
}

/// Locate project configuration. Callers must enforce workspace trust before reading.
#[must_use]
pub fn project_config_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("heycode.toml")
}

/// Locate the project's HeyCode state directory.
#[must_use]
pub fn project_state_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".heycode")
}

fn resolve_home_root(
    configured: Option<std::ffi::OsString>,
    user_home: Option<std::path::PathBuf>,
) -> Result<Option<std::path::PathBuf>, ConfigError> {
    match configured {
        Some(configured) => {
            let root = std::path::PathBuf::from(configured);
            if !root.is_absolute() {
                return Err(ConfigError::Parse {
                    path: "<environment>".to_owned(),
                    message: "HEYCODE_HOME must be an absolute path".to_owned(),
                });
            }
            Ok(Some(normalize_home_path(root)))
        }
        None => Ok(user_home.map(|home| project_state_dir(&home))),
    }
}

/// Resolve `.` and `..` components of an absolute home path lexically.
///
/// `$HEYCODE_HOME=$PWD/../homes/x` is a perfectly ordinary thing to type, yet the
/// owner-only stores under it refuse any path carrying a `..` component. The
/// path is normalised here, once, without touching the filesystem — the
/// directory may not exist yet, and following symlinks is not this function's
/// business.
#[must_use]
pub fn normalize_home_path(path: std::path::PathBuf) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past the root: `/..` stays `/`.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn home_and_project_paths_use_only_heycode() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join(".dshx")).unwrap();
        std::fs::write(root.join("dshx.toml"), "").unwrap();
        assert_eq!(project_config_path(root), root.join("heycode.toml"));
        assert_eq!(project_state_dir(root), root.join(".heycode"));
        assert_eq!(
            resolve_home_root(None, Some(root.to_owned())).unwrap(),
            Some(root.join(".heycode"))
        );
    }

    #[test]
    fn a_home_path_with_dot_components_is_normalised_lexically() {
        let normalised = normalize_home_path(std::path::PathBuf::from("/a/b/../homes/./x/"));
        assert_eq!(normalised, std::path::PathBuf::from("/a/homes/x"));
        assert_eq!(
            normalize_home_path(std::path::PathBuf::from("/../x")),
            std::path::PathBuf::from("/x"),
            "never pops past the root"
        );
        let resolved = resolve_home_root(Some("/a/../b".into()), None)
            .unwrap()
            .unwrap();
        assert_eq!(resolved, std::path::PathBuf::from("/b"));
        assert!(resolve_home_root(Some("relative/x".into()), None).is_err());
    }

    #[test]
    fn home_discovery_refuses_relative_environment_authority() {
        let user_home = std::env::temp_dir().join("safe-user");
        let explicit_home = std::env::temp_dir().join("explicit-heycode");
        let error = resolve_home_root(
            Some(std::ffi::OsString::from("project-controlled-home")),
            Some(user_home.clone()),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("HEYCODE_HOME must be an absolute path")
        );
        assert_eq!(
            resolve_home_root(
                Some(explicit_home.clone().into_os_string()),
                Some(user_home.clone()),
            )
            .unwrap(),
            Some(explicit_home)
        );
        assert_eq!(
            resolve_home_root(None, Some(user_home.clone())).unwrap(),
            Some(user_home.join(".heycode"))
        );
    }

    /// GOTCHAS #6: a `#[serde(default)]` FIELD attribute only fires when the
    /// parent section exists. A present-but-partial section must still inherit
    /// the section's own `Default`, or the documented defaults silently vanish.
    #[test]
    fn partial_sections_inherit_the_section_default_not_zero() {
        let cfg: Config = toml::from_str("[compaction]\nauto = true\n").unwrap();
        assert!(
            (cfg.compaction.threshold_ratio - 0.8).abs() < f32::EPSILON,
            "threshold_ratio fell back to {} instead of 0.8 — auto-compaction \
             would fire on every turn",
            cfg.compaction.threshold_ratio
        );
        assert_eq!(cfg.compaction.context_window, 0);

        let cfg: Config = toml::from_str("[subagent]\n").unwrap();
        assert_eq!(cfg.subagent.max_depth, 3);
        assert_eq!(cfg.subagent.worktree_base, None);

        let cfg: Config = toml::from_str("[llm]\nprovider = \"openrouter\"\n").unwrap();
        assert_eq!(cfg.llm.provider, "openrouter");
        assert_eq!(
            cfg.llm.model, "deepseek-v4-flash",
            "model must fall back to the current DeepSeek default"
        );
        assert_eq!(cfg.llm.protocol, LlmProtocolCfg::Auto);
        assert_eq!(cfg.llm.max_output_tokens, None);

        let cfg: Config =
            toml::from_str("[llm]\nprovider = \"deepseek\"\nprotocol = \"anthropic_messages\"\n")
                .unwrap();
        assert_eq!(cfg.llm.protocol, LlmProtocolCfg::AnthropicMessages);
        let canonical: Config = toml::from_str("[llm]\nprotocol = \"openai_responses\"\n").unwrap();
        let legacy: Config = toml::from_str("[llm]\nprotocol = \"open_ai_responses\"\n").unwrap();
        assert_eq!(canonical.llm.protocol, LlmProtocolCfg::OpenAiResponses);
        assert_eq!(legacy.llm.protocol, LlmProtocolCfg::OpenAiResponses);

        let cfg: Config = toml::from_str("[tools]\nbash_timeout_ms = 5000\n").unwrap();
        assert_eq!(cfg.tools.bash_timeout_ms, 5_000);
        assert_eq!(cfg.tools.read_max_bytes, 262_144);
        assert_eq!(cfg.tools.read_max_lines, 2_000);

        let cfg: Config = toml::from_str("[web]\n").unwrap();
        assert!(cfg.web.enabled, "web defaults on");
    }

    /// Principle #1.4: malformed config fails loud naming the offender.
    #[test]
    fn boolean_patches_reject_garbage_instead_of_coercing_to_false() {
        let mut cfg = Config::defaults();
        let err = cfg.apply_patch("ui.auto_title=maybe").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("ui.auto_title"), "must name the path: {text}");
        assert!(text.contains("maybe"), "must name the bad value: {text}");

        let mut cfg = Config::defaults();
        cfg.apply_patch("ui.auto_title=true").unwrap();
        assert!(cfg.ui.auto_title);
        let mut cfg = Config::defaults();
        cfg.apply_patch("compaction.auto=false").unwrap();
        assert!(!cfg.compaction.auto);

        let mut cfg = Config::defaults();
        cfg.apply_patch("llm.protocol=anthropic_messages").unwrap();
        assert_eq!(cfg.llm.protocol, LlmProtocolCfg::AnthropicMessages);
        assert!(cfg.apply_patch("llm.protocol=compatible").is_err());
        cfg.apply_patch("llm.max_output_tokens=4096").unwrap();
        assert_eq!(cfg.llm.max_output_tokens, Some(4096));
        assert!(cfg.apply_patch("llm.max_output_tokens=0").is_err());
    }

    #[test]
    fn defaults_compose_the_standard_stack() {
        let cfg = Config::defaults();
        assert!(
            cfg.profile.plugins.is_empty(),
            "defaults must defer to the composition root's built-in profile, \
             not fork a second list that goes stale: {:?}",
            cfg.profile.plugins
        );
        assert_eq!(cfg.llm.provider, "deepseek");
        assert_eq!(cfg.llm.model, "deepseek-v4-flash");
        assert_eq!(cfg.llm.protocol, LlmProtocolCfg::Auto);
        assert_eq!(cfg.llm.max_output_tokens, None);
        assert!(
            cfg.resolve_plugins(&[
                "session", "prompt", "tools", "llm", "approval", "commands", "agent", "tui"
            ])
            .is_ok()
        );
    }

    #[test]
    fn parses_full_file_and_fills_defaults_for_missing_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("heycode.toml");
        std::fs::write(
            &path,
            "[profile]\nplugins = [\"session\", \"agent\"]\n\n[llm]\nprovider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-4\"\n",
        )
        .unwrap();
        let cfg = Config::from_file(&path).unwrap();
        assert_eq!(cfg.llm.provider, "openrouter");
        assert_eq!(cfg.tools.bash_timeout_ms, 600_000); // defaulted
        assert_eq!(cfg.tools.foreground_timeout_secs, 120);
        assert!(cfg.web.enabled, "web is native-on by default");
    }

    #[test]
    fn patches_apply_and_unknown_paths_fail_loud() {
        let mut cfg = Config::defaults();
        cfg.apply_patch("llm.model=deepseek-reasoner").unwrap();
        assert_eq!(cfg.llm.model, "deepseek-reasoner");
        cfg.apply_patch("tools.bash_timeout_ms=5000").unwrap();
        assert_eq!(cfg.tools.bash_timeout_ms, 5000);
        assert_eq!(
            cfg.patched_paths().collect::<Vec<_>>(),
            vec!["llm.model", "tools.bash_timeout_ms"],
            "successful patches are remembered so flags can outrank persisted settings"
        );
        assert!(cfg.is_patched("llm.model"));
        assert!(!cfg.is_patched("llm.provider"));
        assert!(cfg.apply_patch("nope.nothing=1").is_err());
        assert!(
            !cfg.is_patched("nope.nothing"),
            "a rejected patch is not recorded"
        );
        assert!(cfg.apply_patch("garbage").is_err());
        assert!(cfg.apply_patch("tools.bash_timeout_ms=abc").is_err());
    }

    #[test]
    fn resolve_plugins_names_offender_and_available() {
        let mut cfg = Config::defaults();
        cfg.profile.plugins = vec!["session".into(), "wat".into()];
        let err = cfg.resolve_plugins(&["session"]).unwrap_err().to_string();
        assert!(err.contains("unknown plugin `wat`"), "{err}");
        assert!(err.contains("available: session"));
    }

    #[test]
    fn approval_mode_parses_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[approval]\nmode = \"deny\"\n").unwrap();
        let cfg = Config::from_file(&path).unwrap();
        assert_eq!(cfg.approval.mode, Some(ApprovalMode::Deny));

        let mut cfg = Config::defaults();
        cfg.apply_patch("approval.mode=deny").unwrap();
        assert_eq!(cfg.approval.mode, Some(ApprovalMode::Deny));
        cfg.apply_patch("approval.mode=full_access").unwrap();
        assert_eq!(cfg.approval.mode, Some(ApprovalMode::FullAccess));
        assert!(cfg.apply_patch("approval.mode=sideways").is_err());
    }

    #[test]
    fn tier2_sections_parse_and_patch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            "[approval]\nmode = \"ask\"\n\n[compaction]\nauto = false\ncontext_window = 8000\n\n[subagent]\nmax_depth = 1\nworktree_base = \"0123456789abcdef0123456789abcdef01234567\"\n",
        )
        .unwrap();
        let cfg = Config::from_file(&path).unwrap();
        assert_eq!(cfg.approval.mode, Some(ApprovalMode::Ask));
        assert!(!cfg.compaction.auto);
        assert_eq!(cfg.compaction.context_window, 8000);
        assert_eq!(cfg.subagent.max_depth, 1);
        assert_eq!(
            cfg.subagent.worktree_base.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );

        let mut cfg = Config::defaults();
        cfg.apply_patch("compaction.context_window=4000").unwrap();
        cfg.apply_patch("subagent.max_depth=2").unwrap();
        cfg.apply_patch("subagent.worktree_base=fedcba9876543210fedcba9876543210fedcba98")
            .unwrap();
        assert_eq!(cfg.compaction.context_window, 4000);
        assert_eq!(cfg.subagent.max_depth, 2);
        assert_eq!(
            cfg.subagent.worktree_base.as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba98")
        );
    }

    #[test]
    fn sandbox_mode_parses_and_patches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[sandbox]\nmode = \"workspace\"\n").unwrap();
        let cfg = Config::from_file(&path).unwrap();
        assert_eq!(cfg.sandbox.mode, SandboxModeCfg::Workspace);

        let mut cfg = Config::defaults();
        assert_eq!(cfg.sandbox.mode, SandboxModeCfg::Off);
        cfg.apply_patch("sandbox.mode=readonly").unwrap();
        assert_eq!(cfg.sandbox.mode, SandboxModeCfg::ReadOnly);
        assert!(cfg.apply_patch("sandbox.mode=jail").is_err());
    }

    #[test]
    fn explicit_file_wins_over_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        std::fs::write(&path, "[llm]\nprovider = \"openrouter\"\nmodel = \"m\"\n").unwrap();
        let cfg = Config::load(Some(&path)).unwrap();
        assert_eq!(cfg.llm.provider, "openrouter");
    }
}
