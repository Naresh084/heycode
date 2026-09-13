//! Lossless, previewable configuration migrations.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use atomic_write_file::AtomicWriteFile;
use toml_edit::{DocumentMut, Item, Value, value};

use crate::{ConfigError, DEFAULT_LLM_MODEL, DEFAULT_LLM_PROVIDER};

/// Current persisted configuration schema.
pub const CONFIG_SCHEMA_VERSION: u32 = 31;

const RETIRED_DEEPSEEK_DEFAULT: &str = "deepseek-chat";

const GENERATED_PROFILE_V0: &[&str] = &[
    "session", "prompt", "tools", "llm", "approval", "commands", "agent", "tui",
];

/// Relationship between a configuration document and this binary's schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigVersionState {
    /// No version marker; this is the historical v0 representation.
    Unversioned,
    /// An explicitly versioned document older than this binary.
    Older(u32),
    /// A document using the schema version supported by this binary.
    Current(u32),
    /// A document written by a newer binary.
    Newer(u32),
}

/// One redacted semantic change in a migration preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigMigrationChange {
    /// Preserve the old unconditional approval setting under its explicit new name.
    RenameLegacyAutoApproval,
    /// Add or advance the root `schema_version` marker.
    SetSchemaVersion {
        /// Prior explicit version; `None` means the document was unversioned.
        from: Option<u32>,
        /// Version written by the migration.
        to: u32,
    },
    /// Replace a known setup-generated plugin snapshot with the live built-in
    /// profile so capabilities added after setup are no longer frozen out.
    UseBuiltinProfile {
        /// Exact historical snapshot that was recognized.
        frozen_plugins: Vec<String>,
        /// Current built-in rows absent from the historical snapshot.
        activated_plugins: Vec<String>,
    },
    /// Materialize a newly explicit dependency inside an older custom complete
    /// profile without changing any of its intentional plugin selections.
    AddRequiredProfilePlugin {
        /// Plugin row inserted into the preserved profile.
        plugin: String,
        /// Existing plugin whose activation requires the inserted row.
        required_by: String,
    },
    /// Preserve a provider-specific authorization flow when ownership moves
    /// out of a shared compatibility plugin.
    MoveAuthorizationFlowToProviderPlugin {
        /// Stable authorization flow id whose behavior is preserved.
        flow: String,
        /// Historical plugin that contributed the flow.
        from_plugin: String,
        /// Provider-owned plugin inserted immediately after the historical row.
        to_plugin: String,
    },
    /// Replace a retired OS credential store selection with the home file store.
    UseHomeCredentialStore,
    /// Replace only the retired model id that was the historical compiled and
    /// setup default for DeepSeek.
    ReplaceRetiredDeepSeekDefault {
        /// Historical default id.
        from: String,
        /// Current catalog-backed default id.
        to: String,
    },
}

/// Result of applying one previously previewed migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationApplyOutcome {
    /// The original file was backed up and the migrated document committed.
    Applied {
        /// Stable backup containing the exact original bytes.
        backup_path: PathBuf,
    },
    /// The same plan had already committed; no file was rewritten.
    AlreadyApplied {
        /// Backup path established by the first application.
        backup_path: PathBuf,
    },
}

/// Safe action for opening a migrated configuration with an older reader.
///
/// Configuration migration is directional. heycode never fabricates a reverse
/// migration from a current document into an older schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigDowngradeGuidance {
    /// The selected reader already understands the document schema.
    Compatible {
        /// Schema carried by the document.
        document_schema: u32,
        /// Newest schema the selected reader understands.
        reader_max_schema: u32,
    },
    /// Restore the byte-exact migration backup before using the older reader.
    RestoreBackup {
        /// Newest schema the selected reader understands.
        reader_max_schema: u32,
        /// Schema of the backup, or `None` for the historical unversioned form.
        backup_schema: Option<u32>,
        /// Exact backup path created by [`ConfigMigrationPlan::apply`].
        backup_path: PathBuf,
    },
    /// No known compatible backup exists; keep the current binary or supply a
    /// separately retained copy written for the selected reader.
    RequiresCompatibleCopy {
        /// Schema carried by the document.
        document_schema: u32,
        /// Newest schema the selected reader understands.
        reader_max_schema: u32,
    },
}

/// Configuration source selected by the discovery precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Path supplied through `--config`.
    Explicit(PathBuf),
    /// `heycode.toml` in the current project.
    Project(PathBuf),
    /// `$HEYCODE_HOME/config.toml` (or the default heycode home).
    Home(PathBuf),
    /// Home file as the base with the project's `heycode.toml` layered over it.
    Layered {
        /// Base document.
        home: PathBuf,
        /// Overlay document.
        project: PathBuf,
    },
    /// No persisted file; use compiled defaults.
    BuiltIn,
}

impl ConfigSource {
    /// Selected file path, or `None` for compiled defaults. A layered source
    /// answers with the overlay: it is the file the user is editing.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Explicit(path) | Self::Project(path) | Self::Home(path) => Some(path),
            Self::Layered { project, .. } => Some(project),
            Self::BuiltIn => None,
        }
    }

    /// Every file that contributed, lowest layer first.
    #[must_use]
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Explicit(path) | Self::Project(path) | Self::Home(path) => vec![path],
            Self::Layered { home, project } => vec![home, project],
            Self::BuiltIn => Vec::new(),
        }
    }
}

/// Whether startup committed a safe migration or only reported one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigMigrationDisposition {
    /// A home-config migration committed after creating this backup.
    Applied {
        /// Byte-exact original document.
        backup_path: PathBuf,
    },
    /// The source is explicit/project-owned and was not changed automatically.
    Pending,
}

/// Redacted startup notice for a planned or applied config migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigrationNotice {
    /// Source file involved.
    pub path: PathBuf,
    /// Version classification before migration.
    pub from: ConfigVersionState,
    /// Current schema written or proposed.
    pub to: u32,
    /// Secret-free semantic changes.
    pub changes: Vec<ConfigMigrationChange>,
    /// Whether startup applied or deferred the change.
    pub disposition: ConfigMigrationDisposition,
}

/// Parsed startup configuration plus source and migration evidence.
pub struct LoadedConfig {
    /// Configuration used by composition.
    pub config: crate::Config,
    /// Winning discovery source.
    pub source: ConfigSource,
    /// Migration performed or awaiting explicit handling.
    pub migration: Option<ConfigMigrationNotice>,
    /// Non-fatal findings (unknown keys) to show once at startup.
    pub warnings: Vec<crate::ConfigWarning>,
}

/// A redacted preview plus the private bytes required for conflict-safe apply.
///
/// The raw documents deliberately have no public accessors or `Debug`
/// implementation, keeping future secret-bearing extension fields out of
/// diagnostics.
pub struct ConfigMigrationPlan {
    path: PathBuf,
    from: ConfigVersionState,
    changes: Vec<ConfigMigrationChange>,
    original: String,
    migrated: String,
}

impl ConfigMigrationPlan {
    /// Read a file and plan its migration without changing disk state.
    ///
    /// `current_builtin_profile` comes from the composition root, which owns
    /// the plugin table. The config crate owns only historical fingerprints.
    ///
    /// # Errors
    /// File I/O, malformed TOML/version fields, or a newer schema fail loud.
    pub fn read(
        path: &Path,
        current_builtin_profile: &[&str],
    ) -> Result<Option<Self>, ConfigError> {
        let canonical = std::fs::canonicalize(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let original = read_to_string(&canonical)?;
        Self::from_raw(canonical, original, current_builtin_profile)
    }

    /// Source path that will be migrated.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Version classification observed when this plan was built.
    #[must_use]
    pub fn from(&self) -> ConfigVersionState {
        self.from
    }

    /// Schema version the plan writes.
    #[must_use]
    pub const fn to(&self) -> u32 {
        CONFIG_SCHEMA_VERSION
    }

    /// Redacted semantic changes shown to a user or diagnostic surface.
    #[must_use]
    pub fn changes(&self) -> &[ConfigMigrationChange] {
        &self.changes
    }

    /// Generate downgrade guidance for a reader with this maximum schema.
    ///
    /// A compatible original is restored from the exact backup that apply
    /// creates. If the original is itself too new, no reverse transformation
    /// is claimed.
    #[must_use]
    pub fn downgrade_guidance(&self, reader_max_schema: u32) -> ConfigDowngradeGuidance {
        if reader_max_schema >= CONFIG_SCHEMA_VERSION {
            return ConfigDowngradeGuidance::Compatible {
                document_schema: CONFIG_SCHEMA_VERSION,
                reader_max_schema,
            };
        }
        let (backup_schema, backup_floor) = match self.from {
            ConfigVersionState::Unversioned => (None, 0),
            ConfigVersionState::Older(version)
            | ConfigVersionState::Current(version)
            | ConfigVersionState::Newer(version) => (Some(version), version),
        };
        if reader_max_schema >= backup_floor {
            ConfigDowngradeGuidance::RestoreBackup {
                reader_max_schema,
                backup_schema,
                backup_path: backup_path(&self.path, self.from),
            }
        } else {
            ConfigDowngradeGuidance::RequiresCompatibleCopy {
                document_schema: CONFIG_SCHEMA_VERSION,
                reader_max_schema,
            }
        }
    }

    /// Apply this plan with a byte-exact backup and atomic destination replace.
    ///
    /// The source is compared with the previewed bytes immediately before the
    /// commit. Concurrent edits are refused rather than overwritten.
    ///
    /// # Errors
    /// I/O failures, a conflicting backup, or source changes since preview.
    pub fn apply(&self) -> Result<MigrationApplyOutcome, ConfigError> {
        let backup_path = backup_path(&self.path, self.from);
        let current = read_to_string(&self.path)?;
        if current == self.migrated {
            return Ok(MigrationApplyOutcome::AlreadyApplied { backup_path });
        }
        if current != self.original {
            return Err(ConfigError::MigrationConflict {
                path: self.path.display().to_string(),
                message: "the file changed after the migration preview; preview it again"
                    .to_owned(),
            });
        }

        ensure_backup(&self.path, &backup_path, &self.original)?;

        // Close the race between backup creation and destination commit.
        if read_to_string(&self.path)? != self.original {
            return Err(ConfigError::MigrationConflict {
                path: self.path.display().to_string(),
                message:
                    "the file changed while its backup was being created; no migration was written"
                        .to_owned(),
            });
        }

        let mut output = AtomicWriteFile::open(&self.path).map_err(|source| ConfigError::Io {
            path: self.path.display().to_string(),
            source,
        })?;
        output
            .write_all(self.migrated.as_bytes())
            .map_err(|source| ConfigError::Io {
                path: self.path.display().to_string(),
                source,
            })?;
        output.commit().map_err(|source| ConfigError::Io {
            path: self.path.display().to_string(),
            source,
        })?;

        Ok(MigrationApplyOutcome::Applied { backup_path })
    }

    pub(crate) fn notice(&self, disposition: ConfigMigrationDisposition) -> ConfigMigrationNotice {
        ConfigMigrationNotice {
            path: self.path.clone(),
            from: self.from,
            to: CONFIG_SCHEMA_VERSION,
            changes: self.changes.clone(),
            disposition,
        }
    }

    fn from_raw(
        path: PathBuf,
        original: String,
        current_builtin_profile: &[&str],
    ) -> Result<Option<Self>, ConfigError> {
        let label = path.display().to_string();
        let mut document = parse_document(&original, &label)?;
        let from = classify_parsed(&document, &label)?;
        match from {
            ConfigVersionState::Current(_) => return Ok(None),
            ConfigVersionState::Newer(found) => {
                return Err(ConfigError::NewerSchema {
                    path: label,
                    found,
                    supported: CONFIG_SCHEMA_VERSION,
                });
            }
            ConfigVersionState::Unversioned | ConfigVersionState::Older(_) => {}
        }

        let prior = match from {
            ConfigVersionState::Older(version) => Some(version),
            ConfigVersionState::Unversioned => None,
            ConfigVersionState::Current(_) | ConfigVersionState::Newer(_) => None,
        };
        let mut changes = vec![ConfigMigrationChange::SetSchemaVersion {
            from: prior,
            to: CONFIG_SCHEMA_VERSION,
        }];

        // Before v30, `auto` meant unconditional approval. Preserve that
        // explicit setting as Full access; v30 `auto` is reserved for AI review.
        if let Some(mode) = document
            .get_mut("approval")
            .and_then(Item::as_table_mut)
            .and_then(|approval| approval.get_mut("mode"))
            && mode.as_str() == Some("auto")
        {
            let mut replacement = Value::from("full_access");
            if let Some(previous) = mode.as_value() {
                *replacement.decor_mut() = previous.decor().clone();
            }
            *mode = Item::Value(replacement);
            changes.push(ConfigMigrationChange::RenameLegacyAutoApproval);
        }

        if generated_profile(&document).is_some() {
            let activated_plugins = current_builtin_profile
                .iter()
                .copied()
                .filter(|name| !GENERATED_PROFILE_V0.contains(name))
                .map(str::to_owned)
                .collect();
            changes.push(ConfigMigrationChange::UseBuiltinProfile {
                frozen_plugins: GENERATED_PROFILE_V0
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                activated_plugins,
            });
            document.remove("profile");
        }

        if use_home_credential_store(&mut document) {
            changes.push(ConfigMigrationChange::UseHomeCredentialStore);
        }

        if add_required_profile_plugin(&mut document, "agent", "agent-options") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "agent-options".to_owned(),
                required_by: "agent".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "agent", "models") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "models".to_owned(),
                required_by: "agent".to_owned(),
            });
        }
        // v23 -> v24: the Anthropic counter consumes the same policy namespace
        // as inference, so an exact counter-only profile gains that owner and
        // its authority/service prerequisites before the counter.
        for dependency in ["authorization", "settings", "provider-anthropic"] {
            if add_required_profile_plugin(&mut document, "token-count-anthropic", dependency) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: dependency.to_owned(),
                    required_by: "token-count-anthropic".to_owned(),
                });
            }
        }
        // v23 -> v24: provider-owned cache/context policy namespaces register
        // inside their provider plugins and must resolve before inference or
        // the aligned Anthropic token counter can be constructed.
        for owner in ["provider-anthropic", "provider-openai"] {
            if add_required_profile_plugin(&mut document, owner, "settings") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "settings".to_owned(),
                    required_by: owner.to_owned(),
                });
            }
        }
        // v27 -> v28: hosted/server-tool policy now resolves from the same
        // registered Settings snapshot that publishes N01 candidates. Exact
        // OpenAI/Anthropic profiles therefore need the provider-owned policy
        // plugin before `llm`; cloud inference rows need their dedicated
        // policy namespace owner. No default is reconstructed inside an
        // inference apply path when a policy plugin was intentionally omitted.
        for (provider, policy_plugin) in [
            ("openai", "native-openai"),
            ("anthropic", "native-anthropic"),
        ] {
            if uses_provider(&document, provider) {
                for dependency in ["settings", "native-tools", policy_plugin] {
                    if add_required_profile_plugin(&mut document, "llm", dependency) {
                        changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                            plugin: dependency.to_owned(),
                            required_by: "llm".to_owned(),
                        });
                    }
                }
            }
        }
        for (inference, settings_owner) in [
            ("inference-bedrock-converse", "settings-aws-bedrock"),
            ("inference-google-gemini", "settings-google-inference"),
            ("inference-google-vertex", "settings-google-inference"),
        ] {
            if add_required_profile_plugin(&mut document, inference, settings_owner) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: settings_owner.to_owned(),
                    required_by: inference.to_owned(),
                });
            }
            if add_required_profile_plugin(&mut document, settings_owner, "settings") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "settings".to_owned(),
                    required_by: settings_owner.to_owned(),
                });
            }
        }
        // v24 -> v25: O05 attaches background task settlement only after the
        // subagent registry and primary Agent/job owner both exist. R05/R08
        // similarly adapt only the delegated runtime rows an exact profile
        // already selected; unrelated minimal profiles gain nothing.
        if first_profile_plugin(&document, &["subagent"]).is_some()
            && first_profile_plugin(&document, &["agent"]).is_some()
            && add_profile_plugin_after(&mut document, "agent", "subagent-jobs")
        {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "subagent-jobs".to_owned(),
                required_by: "subagent".to_owned(),
            });
        }
        for (runtime, bridge) in [
            ("runtime-codex", "subagent-codex"),
            ("runtime-claude", "subagent-claude"),
        ] {
            if first_profile_plugin(&document, &["subagent"]).is_some()
                && first_profile_plugin(&document, &[runtime]).is_some()
                && add_profile_plugin_after(&mut document, "subagent", bridge)
            {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: bridge.to_owned(),
                    required_by: runtime.to_owned(),
                });
            }
        }
        // v19 -> v20: C11 makes token measurement a required Agent/subagent
        // dependency. Insert once before whichever Consumer appears first so
        // an exact profile preserves its relative order and still composes.
        if let Some(required_by) = first_profile_plugin(&document, &["agent", "subagent"])
            && add_required_profile_plugin(&mut document, &required_by, "token-counters")
        {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "token-counters".to_owned(),
                required_by,
            });
        }
        // v21 -> v22: C12 makes the strategy registry a required dependency of
        // both the primary Agent and native product subagents.
        if let Some(required_by) = first_profile_plugin(&document, &["agent", "subagent"])
            && add_required_profile_plugin(&mut document, &required_by, "compactions")
        {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "compactions".to_owned(),
                required_by,
            });
        }
        if add_required_profile_plugin(&mut document, "tui", "ui") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "ui".to_owned(),
                required_by: "tui".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "tui", "runtimes") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "runtimes".to_owned(),
                required_by: "tui".to_owned(),
            });
        }
        // v20 -> v21: TUI owns the real K07 picker command and therefore
        // requires both the shared profile service and command registry.
        for plugin in ["profiles", "commands"] {
            if add_required_profile_plugin(&mut document, "tui", plugin) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: plugin.to_owned(),
                    required_by: "tui".to_owned(),
                });
            }
        }
        // v22 -> v23: U15/CMD06 moves the session browser and lifecycle
        // commands into TUI, whose handle now requires the replaceable query
        // owner even when an exact profile omitted that formerly optional row.
        if add_required_profile_plugin(&mut document, "tui", "session-query-jsonl") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "session-query-jsonl".to_owned(),
                required_by: "tui".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "tui", "routing") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "routing".to_owned(),
                required_by: "tui".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "routing", "settings") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "settings".to_owned(),
                required_by: "routing".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "routing", "models") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "models".to_owned(),
                required_by: "routing".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "routing", "runtime-native") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "runtime-native".to_owned(),
                required_by: "routing".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "tui", "app-server") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "app-server".to_owned(),
                required_by: "tui".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "tools", "shell-local") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "shell-local".to_owned(),
                required_by: "tools".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "shell-local", "workspace-scope") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "workspace-scope".to_owned(),
                required_by: "shell-local".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "shell-local", "subprocess-local") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "subprocess-local".to_owned(),
                required_by: "shell-local".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "mcp", "subprocess-local") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "subprocess-local".to_owned(),
                required_by: "mcp".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "subprocess-local", "sandbox") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "sandbox".to_owned(),
                required_by: "subprocess-local".to_owned(),
            });
        }
        if add_required_profile_plugin(&mut document, "tools", "filesystem-local") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "filesystem-local".to_owned(),
                required_by: "tools".to_owned(),
            });
        }
        for required_by in ["tools", "agent", "subagent"] {
            if add_required_profile_plugin(&mut document, required_by, "native-tools") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "native-tools".to_owned(),
                    required_by: required_by.to_owned(),
                });
            }
        }
        if web_enabled(&document) {
            if add_required_profile_plugin(&mut document, "tools", "web") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "web".to_owned(),
                    required_by: "tools".to_owned(),
                });
            }
            if add_profile_plugin_after(&mut document, "web", "web-portable") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "web-portable".to_owned(),
                    required_by: "tools".to_owned(),
                });
            }
        }
        if add_required_profile_plugin(&mut document, "mcp", "mcp-registry") {
            changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                plugin: "mcp-registry".to_owned(),
                required_by: "mcp".to_owned(),
            });
        }
        if add_profile_plugin_after(
            &mut document,
            "authorization-api-key",
            "provider-openrouter",
        ) {
            changes.push(
                ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin {
                    flow: "openrouter-api-key".to_owned(),
                    from_plugin: "authorization-api-key".to_owned(),
                    to_plugin: "provider-openrouter".to_owned(),
                },
            );
        }
        if uses_provider(&document, "openrouter") {
            if add_required_profile_plugin(&mut document, "llm", "native-tools") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "native-tools".to_owned(),
                    required_by: "native-openrouter".to_owned(),
                });
            }
            if add_profile_plugin_after(&mut document, "native-tools", "native-openrouter") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "native-openrouter".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
            for plugin in ["http", "models"] {
                if add_required_profile_plugin(&mut document, "llm", plugin) {
                    changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                        plugin: plugin.to_owned(),
                        required_by: "catalog-openrouter".to_owned(),
                    });
                }
            }
            if add_profile_plugin_after_dependencies(
                &mut document,
                &["http", "models", "llm"],
                "catalog-openrouter",
            ) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "catalog-openrouter".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
        }

        // v17 -> v18: the Anthropic provider owns its own authorization flow
        // and catalog. Only an exact profile that already selects Anthropic
        // gains them, so an intentional profile that never used Anthropic is
        // left alone.
        if uses_provider(&document, "anthropic") {
            for plugin in ["http", "models"] {
                if add_required_profile_plugin(&mut document, "llm", plugin) {
                    changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                        plugin: plugin.to_owned(),
                        required_by: "catalog-anthropic".to_owned(),
                    });
                }
            }
            if add_profile_plugin_after(&mut document, "authorization", "provider-anthropic") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "provider-anthropic".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
            if add_profile_plugin_after_dependencies(
                &mut document,
                &["http", "models", "llm"],
                "catalog-anthropic",
            ) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "catalog-anthropic".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
        }

        // v18 -> v19: the OpenAI provider owns its own authorization flow and
        // catalog, exactly as Anthropic does. Same rule and same reason: only
        // an exact profile that already selects OpenAI gains them, so an
        // intentional profile that never used OpenAI is left alone.
        //
        // The other plugins added to the default set in this change
        // (`authorization-aws`, `authorization-gcp`, `provider-lmstudio`,
        // `telemetry-local-off`) deliberately get **no** migration: each
        // publishes a service nothing else injects, so an existing exact
        // profile still composes without them. Migrating a profile to add a
        // plugin nothing depends on is a change users pay for and nobody uses.
        if uses_provider(&document, "openai") {
            for plugin in ["http", "models"] {
                if add_required_profile_plugin(&mut document, "llm", plugin) {
                    changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                        plugin: plugin.to_owned(),
                        required_by: "catalog-openai".to_owned(),
                    });
                }
            }
            if add_profile_plugin_after(&mut document, "authorization", "provider-openai") {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "provider-openai".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
            if add_profile_plugin_after_dependencies(
                &mut document,
                &["http", "models", "llm"],
                "catalog-openai",
            ) {
                changes.push(ConfigMigrationChange::AddRequiredProfilePlugin {
                    plugin: "catalog-openai".to_owned(),
                    required_by: "llm".to_owned(),
                });
            }
        }

        if uses_retired_deepseek_default(&document) {
            changes.push(ConfigMigrationChange::ReplaceRetiredDeepSeekDefault {
                from: RETIRED_DEEPSEEK_DEFAULT.to_owned(),
                to: DEFAULT_LLM_MODEL.to_owned(),
            });
            replace_model_preserving_decor(&mut document, DEFAULT_LLM_MODEL);
        }

        document.insert("schema_version", value(i64::from(CONFIG_SCHEMA_VERSION)));
        let migrated = document.to_string();
        Ok(Some(Self {
            path,
            from,
            changes,
            original,
            migrated,
        }))
    }
}

pub(crate) fn classify_document(
    raw: &str,
    path_label: &str,
) -> Result<ConfigVersionState, ConfigError> {
    let document = parse_document(raw, path_label)?;
    classify_parsed(&document, path_label)
}

fn classify_parsed(
    document: &DocumentMut,
    path_label: &str,
) -> Result<ConfigVersionState, ConfigError> {
    let Some(item) = document.get("schema_version") else {
        return Ok(ConfigVersionState::Unversioned);
    };
    let Some(raw_version) = item.as_integer() else {
        return Err(ConfigError::Parse {
            path: path_label.to_owned(),
            message: "`schema_version` must be a non-negative integer".to_owned(),
        });
    };
    let version = u32::try_from(raw_version).map_err(|_| ConfigError::Parse {
        path: path_label.to_owned(),
        message: "`schema_version` is outside the supported integer range".to_owned(),
    })?;
    Ok(match version.cmp(&CONFIG_SCHEMA_VERSION) {
        std::cmp::Ordering::Less => ConfigVersionState::Older(version),
        std::cmp::Ordering::Equal => ConfigVersionState::Current(version),
        std::cmp::Ordering::Greater => ConfigVersionState::Newer(version),
    })
}

fn parse_document(raw: &str, path_label: &str) -> Result<DocumentMut, ConfigError> {
    raw.parse::<DocumentMut>()
        .map_err(|error| ConfigError::Parse {
            path: path_label.to_owned(),
            message: error.message().to_owned(),
        })
}

fn generated_profile(document: &DocumentMut) -> Option<()> {
    let profile = document.get("profile")?.as_table()?;
    if profile.len() != 1 {
        return None;
    }
    let plugins = profile.get("plugins")?.as_array()?;
    if plugins.len() != GENERATED_PROFILE_V0.len() {
        return None;
    }
    plugins
        .iter()
        .zip(GENERATED_PROFILE_V0)
        .all(|(actual, expected)| actual.as_str() == Some(expected))
        .then_some(())
}

fn use_home_credential_store(document: &mut DocumentMut) -> bool {
    let Some(plugins) = document
        .get_mut("profile")
        .and_then(Item::as_table_mut)
        .and_then(|profile| profile.get_mut("plugins"))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    let Some(index) = plugins
        .iter()
        .position(|row| row.as_str() == Some("credentials-keychain"))
    else {
        return false;
    };
    let has_file = plugins
        .iter()
        .any(|row| row.as_str() == Some("credentials-file"));
    if !has_file {
        plugins.insert(index, "credentials-file");
    }
    plugins.retain(|row| row.as_str() != Some("credentials-keychain"));
    true
}

fn add_required_profile_plugin(
    document: &mut DocumentMut,
    required_by: &str,
    plugin: &str,
) -> bool {
    let Some(plugins) = document
        .get_mut("profile")
        .and_then(Item::as_table_mut)
        .and_then(|profile| profile.get_mut("plugins"))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    if plugins.iter().any(|row| row.as_str() == Some(plugin)) {
        return false;
    }
    let Some(required_by_index) = plugins
        .iter()
        .position(|row| row.as_str() == Some(required_by))
    else {
        return false;
    };
    plugins.insert(required_by_index, plugin);
    true
}

fn first_profile_plugin(document: &DocumentMut, candidates: &[&str]) -> Option<String> {
    document
        .get("profile")
        .and_then(Item::as_table)
        .and_then(|profile| profile.get("plugins"))
        .and_then(Item::as_array)
        .and_then(|plugins| {
            plugins
                .iter()
                .filter_map(Value::as_str)
                .find(|plugin| candidates.contains(plugin))
        })
        .map(str::to_owned)
}

fn add_profile_plugin_after(document: &mut DocumentMut, after: &str, plugin: &str) -> bool {
    let Some(plugins) = document
        .get_mut("profile")
        .and_then(Item::as_table_mut)
        .and_then(|profile| profile.get_mut("plugins"))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    if plugins.iter().any(|row| row.as_str() == Some(plugin)) {
        return false;
    }
    let Some(after_index) = plugins.iter().position(|row| row.as_str() == Some(after)) else {
        return false;
    };
    plugins.insert(after_index + 1, plugin);
    true
}

fn add_profile_plugin_after_dependencies(
    document: &mut DocumentMut,
    dependencies: &[&str],
    plugin: &str,
) -> bool {
    let Some(plugins) = document
        .get_mut("profile")
        .and_then(Item::as_table_mut)
        .and_then(|profile| profile.get_mut("plugins"))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    if plugins.iter().any(|row| row.as_str() == Some(plugin)) {
        return false;
    }
    let Some(after_index) = plugins
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            row.as_str()
                .is_some_and(|value| dependencies.contains(&value))
                .then_some(index)
        })
        .max()
    else {
        return false;
    };
    plugins.insert(after_index + 1, plugin);
    true
}

fn uses_retired_deepseek_default(document: &DocumentMut) -> bool {
    if !is_setup_generated_document(document) {
        return false;
    }
    let Some(llm) = document.get("llm").and_then(Item::as_table) else {
        return false;
    };
    let provider = llm
        .get("provider")
        .and_then(Item::as_str)
        .unwrap_or(DEFAULT_LLM_PROVIDER);
    provider == DEFAULT_LLM_PROVIDER
        && llm.get("model").and_then(Item::as_str) == Some(RETIRED_DEEPSEEK_DEFAULT)
}

fn uses_provider(document: &DocumentMut, expected: &str) -> bool {
    document
        .get("llm")
        .and_then(Item::as_table)
        .and_then(|llm| llm.get("provider"))
        .and_then(Item::as_str)
        .unwrap_or(DEFAULT_LLM_PROVIDER)
        == expected
}

fn web_enabled(document: &DocumentMut) -> bool {
    document
        .get("web")
        .and_then(Item::as_table)
        .and_then(|web| web.get("enabled"))
        .and_then(Item::as_bool)
        .unwrap_or(true)
}

fn is_setup_generated_document(document: &DocumentMut) -> bool {
    let root = document.as_table();
    if !root
        .iter()
        .all(|(name, _)| matches!(name, "schema_version" | "profile" | "llm" | "tools"))
    {
        return false;
    }
    let Some(llm) = root.get("llm").and_then(Item::as_table) else {
        return false;
    };
    if llm.len() != 2
        || llm.get("provider").and_then(Item::as_str) != Some(DEFAULT_LLM_PROVIDER)
        || llm.get("model").and_then(Item::as_str) != Some(RETIRED_DEEPSEEK_DEFAULT)
    {
        return false;
    }
    let Some(tools) = root.get("tools").and_then(Item::as_table) else {
        return false;
    };
    tools.len() == 3
        && tools.get("bash_timeout_ms").and_then(Item::as_integer) == Some(30_000)
        && tools.get("read_max_bytes").and_then(Item::as_integer) == Some(262_144)
        && tools.get("read_max_lines").and_then(Item::as_integer) == Some(2_000)
        && root
            .get("profile")
            .is_none_or(|_| generated_profile(document).is_some())
}

fn replace_model_preserving_decor(document: &mut DocumentMut, model: &str) {
    let Some(item) = document
        .get_mut("llm")
        .and_then(Item::as_table_mut)
        .and_then(|llm| llm.get_mut("model"))
    else {
        return;
    };
    let Some(previous) = item.as_value() else {
        return;
    };
    let decor = previous.decor().clone();
    let mut replacement = Value::from(model);
    *replacement.decor_mut() = decor;
    *item = Item::Value(replacement);
}

fn backup_path(path: &Path, from: ConfigVersionState) -> PathBuf {
    let label = match from {
        ConfigVersionState::Unversioned => "unversioned".to_owned(),
        ConfigVersionState::Older(version) => format!("v{version}"),
        ConfigVersionState::Current(version) | ConfigVersionState::Newer(version) => {
            format!("v{version}")
        }
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_owned());
    path.with_file_name(format!("{name}.{label}.bak"))
}

fn ensure_backup(
    source_path: &Path,
    backup_path: &Path,
    original: &str,
) -> Result<(), ConfigError> {
    match std::fs::read_to_string(backup_path) {
        Ok(existing) if existing == original => return Ok(()),
        Ok(_) => {
            return Err(ConfigError::MigrationConflict {
                path: backup_path.display().to_string(),
                message:
                    "an existing migration backup has different contents; move it aside and retry"
                        .to_owned(),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ConfigError::Io {
                path: backup_path.display().to_string(),
                source,
            });
        }
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mode = std::fs::metadata(source_path)
            .map_err(|source| ConfigError::Io {
                path: source_path.display().to_string(),
                source,
            })?
            .permissions()
            .mode();
        options.mode(mode);
    }
    let mut backup = match options.open(backup_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_to_string(backup_path)?;
            if existing == original {
                return Ok(());
            }
            return Err(ConfigError::MigrationConflict {
                path: backup_path.display().to_string(),
                message: "another process created a different migration backup".to_owned(),
            });
        }
        Err(source) => {
            return Err(ConfigError::Io {
                path: backup_path.display().to_string(),
                source,
            });
        }
    };
    if let Err(source) = backup
        .write_all(original.as_bytes())
        .and_then(|()| backup.sync_all())
    {
        return Err(ConfigError::Io {
            path: backup_path.display().to_string(),
            source,
        });
    }
    Ok(())
}

fn read_to_string(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Interpret old explicit/project files without rewriting their owned bytes.
pub(crate) fn normalize_legacy_approval(table: &mut toml::Table) {
    let version = table
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(0);
    if version < 30
        && let Some(mode) = table
            .get_mut("approval")
            .and_then(toml::Value::as_table_mut)
            .and_then(|approval| approval.get_mut("mode"))
        && mode.as_str() == Some("auto")
    {
        *mode = toml::Value::String("full_access".into());
    }
}
