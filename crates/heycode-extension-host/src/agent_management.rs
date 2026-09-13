//! Human-only custom-agent authoring and atomic live reload.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::user_declarations::{
    UserDeclarationRoots, load_user_declarations_with_imports, validate_agent_document,
};
use heycode_agent::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandRegistry, CommandSource,
    CommandTiming, SubagentPresetRegistration, SubagentRegistry, UiEvent,
};
use heycode_core::{Context, CoreError, ServiceKey};

/// Service for native and graphical custom-agent controls.
pub const SERVICE_AGENT_DECLARATIONS: ServiceKey = ServiceKey::new("agent-declarations");

/// Owns one generation of file presets. Reloads do not change running children.
pub struct AgentDeclarationService {
    roots: UserDeclarationRoots,
    imported: Vec<heycode_agent::SubagentPreset>,
    registry: Arc<SubagentRegistry>,
    state: Mutex<Option<Vec<SubagentPresetRegistration>>>,
    diagnostics: Mutex<Vec<String>>,
}

impl AgentDeclarationService {
    /// Bind an empty, owned generation.
    #[must_use]
    pub fn new(roots: UserDeclarationRoots, registry: Arc<SubagentRegistry>) -> Self {
        Self::with_imported_presets(roots, registry, Vec::new())
    }

    /// Bind exact imported presets from the same composition generation.
    #[must_use]
    pub fn with_imported_presets(
        roots: UserDeclarationRoots,
        registry: Arc<SubagentRegistry>,
        imported: Vec<heycode_agent::SubagentPreset>,
    ) -> Self {
        Self {
            roots,
            imported,
            registry,
            state: Mutex::new(Some(Vec::new())),
            diagnostics: Mutex::new(Vec::new()),
        }
    }

    /// Diagnostics from the most recent activation/reload attempt.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_else(|_| vec!["agent diagnostics unavailable".to_owned()])
    }

    fn record_diagnostics(&self, rows: Vec<String>) {
        if let Ok(mut diagnostics) = self.diagnostics.lock() {
            *diagnostics = rows;
        }
    }

    /// Validate every agent file, then atomically replace the previous generation.
    /// Invalid input leaves all previous presets intact.
    ///
    /// # Errors
    /// Malformed input, registry conflicts or disposed service.
    pub fn reload(&self) -> Result<String, String> {
        let loaded = load_user_declarations_with_imports(&self.roots, &self.imported);
        let errors: Vec<_> = loaded
            .skipped
            .iter()
            .filter(|row| row.path.starts_with("agents/"))
            .map(|row| format!("{}: {}", row.path, row.reason))
            .collect();
        if !errors.is_empty() {
            self.record_diagnostics(errors.clone());
            return Err(format!(
                "reload refused; existing agents retained\n{}",
                errors.join("\n")
            ));
        }
        let count = loaded.presets.len();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "agent declaration state unavailable")?;
        let owned = state
            .as_mut()
            .ok_or("agent declaration service is closed")?;
        self.registry
            .replace_presets_owned(owned, loaded.presets)
            .map_err(|error| {
                self.record_diagnostics(vec![error.to_string()]);
                error.to_string()
            })?;
        self.record_diagnostics(
            loaded
                .skipped
                .iter()
                .map(|row| format!("{}: {}", row.path, row.reason))
                .collect(),
        );
        Ok(format!(
            "reloaded {count} agent ids (including scope aliases); running children retain their configuration"
        ))
    }

    /// Save validated native JSON under the user's agents directory. This does
    /// not publish changes; use reload after reviewing the file.
    ///
    /// # Errors
    /// Invalid name/document, missing home, symlink or filesystem failure.
    pub fn save(&self, name: &str, document: &str, replace: bool) -> Result<PathBuf, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "agent declaration state unavailable")?;
        if state.is_none() {
            return Err("agent declaration service is closed".to_owned());
        }
        heycode_agent::SubagentPresetId::new(name).map_err(|_| "agent name must be kebab-case")?;
        validate_agent_document(document)?;
        let home = self
            .roots
            .user_home
            .as_ref()
            .ok_or("user home unavailable")?;
        let directory = home.join("agents");
        if std::fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("agents directory must not be a symlink".to_owned());
        }
        std::fs::create_dir_all(&directory).map_err(|_| "cannot create agents directory")?;
        let path = directory.join(format!("{name}.json"));
        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err("agent destination must be a regular file".to_owned());
        }
        let mut temp =
            tempfile::NamedTempFile::new_in(&directory).map_err(|_| "cannot stage agent file")?;
        use std::io::Write;
        temp.write_all(document.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|_| "cannot write agent file")?;
        if replace {
            temp.persist(&path)
                .map_err(|_| "cannot replace agent file")?;
        } else {
            temp.persist_noclobber(&path)
                .map_err(|_| "agent already exists or cannot be created")?;
        }
        Ok(path)
    }

    /// Withdraw all owned rows and make held management handles terminal.
    pub fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = None;
        }
    }
}

pub(crate) fn read_bounded(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "cannot inspect agent file")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > crate::MAX_TEXT_BYTES as u64
    {
        return Err("agent file must be regular, not a symlink, and at most 1 MiB".to_owned());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|_| "cannot open agent file")?
        .take(crate::MAX_TEXT_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "cannot read UTF-8 agent file")?;
    if text.len() > crate::MAX_TEXT_BYTES {
        return Err("agent file exceeds 1 MiB".to_owned());
    }
    Ok(text)
}

struct AgentConfigCommand {
    service: Arc<AgentDeclarationService>,
    descriptor: CommandDescriptor,
}
#[async_trait::async_trait]
impl Command for AgentConfigCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let (action, rest) = args.trim().split_once(' ').unwrap_or((args.trim(), ""));
        let result: Result<String, String> = (|| {
            match action {
            "" | "list" => {
                let loaded = load_user_declarations_with_imports(&self.service.roots, &self.service.imported);
                let mut lines: Vec<_> = self.service.registry.presets().iter().map(|preset| format!("{} — {}", preset.id(), preset.display())).collect();
                lines.extend(loaded.skipped.iter().map(|row| format!("skipped {}: {}", row.path, row.reason)));
                lines.extend(self.service.diagnostics());
                Ok(lines.join("\n"))
            }
            "show" => {
                let preset = self.service.registry.preset(rest.trim()).ok_or("unknown agent id")?;
                let mode = match (preset.seed(), preset.continuation()) {
                    (heycode_agent::SubagentSeed::Fresh, heycode_agent::SubagentContinuation::OneShot) => crate::AgentModeDocument::OneShot,
                    (heycode_agent::SubagentSeed::Fresh, heycode_agent::SubagentContinuation::Continuable) => crate::AgentModeDocument::Continuable,
                    (heycode_agent::SubagentSeed::ForkParent, heycode_agent::SubagentContinuation::OneShot) => crate::AgentModeDocument::Fork,
                    (heycode_agent::SubagentSeed::ForkParent, heycode_agent::SubagentContinuation::Continuable) => crate::AgentModeDocument::ForkContinuable,
                };
                serde_json::to_string_pretty(&crate::AgentDocument { display:preset.display().to_owned(), description:preset.description().map(str::to_owned), instructions:preset.instructions().to_owned(), provider:preset.provider().map(|id|id.as_str().to_owned()), mode, config:preset.config().clone() }).map_err(|_| "cannot render agent configuration".to_owned())
            }
            "reload" => self.service.reload(),
            "validate" => { validate_agent_document(&read_bounded(Path::new(rest.trim()))?)?; Ok("agent document is valid".to_owned()) }
            "create" => {
                let document = serde_json::json!({"display": rest.trim(), "instructions": "Describe this agent's responsibilities and constraints.", "config": {"permissions":"read_only"}}).to_string();
                self.service.save(rest.trim(), &document, false).map(|path| format!("created {}; edit then /agent-config reload", path.display()))
            }
            "save" => {
                let (name, document) = rest.split_once(' ').ok_or("usage: /agent-config save <name> <JSON>")?;
                self.service.save(name, document, true).map(|path| format!("saved {}; /agent-config reload to activate", path.display()))
            }
            "import" => {
                let mut args = rest.splitn(3, ' ');
                let format = match args.next() { Some("claude") => crate::AgentImportFormat::Claude, Some("codex") => crate::AgentImportFormat::Codex, _ => return Err("import format must be claude or codex".to_owned()) };
                let name = args.next().ok_or("import needs a destination name")?;
                let path = args.next().ok_or("import needs a source path")?;
                let document = crate::import_agent(&read_bounded(Path::new(path))?, format)?;
                self.service.save(name, &document, false).map(|path| format!("imported {}; inspect and /agent-config reload", path.display()))
            }
            _ => Err("usage: /agent-config list|show <id>|reload|create <name>|validate <path>|save <name> <JSON>|import <claude|codex> <name> <path>".to_owned()),
        }
        })();
        agent.ui().emit(UiEvent::Info {
            text: result
                .unwrap_or_else(|error| error)
                .chars()
                .map(|c| {
                    if c.is_control() && c != '\n' && c != '\t' {
                        '\u{fffd}'
                    } else {
                        c
                    }
                })
                .collect(),
        });
        Ok(())
    }
}

pub(crate) fn install(
    context: &mut Context,
    roots: UserDeclarationRoots,
    registry: Arc<SubagentRegistry>,
    initial: crate::user_declarations::UserDeclarations,
    imported: Vec<heycode_agent::SubagentPreset>,
) -> Result<(), CoreError> {
    context.contribute(heycode_core::ContributionKind::Command, "agent-config")?;
    context.provide(
        SERVICE_AGENT_DECLARATIONS,
        crate::PRODUCT_EXTENSIONS_PLUGIN_ID,
        AgentDeclarationService::with_imported_presets(roots, registry, imported),
    )?;
    let service = context
        .get::<AgentDeclarationService>(SERVICE_AGENT_DECLARATIONS)
        .ok_or_else(|| CoreError::other("agent declaration service unavailable"))?;
    {
        let mut state = service
            .state
            .lock()
            .map_err(|_| CoreError::other("agent declaration state unavailable"))?;
        if let Some(owned) = state.as_mut() {
            let ids: Vec<_> = initial
                .presets
                .iter()
                .map(|preset| preset.id().as_str().to_owned())
                .collect();
            let mut diagnostics: Vec<_> = initial
                .skipped
                .iter()
                .map(|row| format!("{}: {}", row.path, row.reason))
                .collect();
            match service
                .registry
                .replace_presets_owned(owned, initial.presets)
            {
                Ok(()) => {
                    for id in ids {
                        context.contribute(heycode_core::ContributionKind::AgentPreset, id)?;
                    }
                }
                Err(error) => diagnostics.push(format!("file agents were not activated: {error}")),
            }
            service.record_diagnostics(diagnostics);
        }
    }
    let descriptor = CommandDescriptor::new(
        "agent-config",
        "Author, validate, import and reload custom agents",
        vec![
            CommandArgument::optional(
                "action",
                "list, show, reload, create, validate, save, import",
            )
            .map_err(|error| CoreError::other(error.to_string()))?
            .variadic(),
        ],
        CommandTiming::Immediate,
        CommandSource::from_plugin("product-extensions")
            .map_err(|error| CoreError::other(error.to_string()))?,
    )
    .map_err(|error| CoreError::other(error.to_string()))?;
    if let Some(commands) = context.get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS) {
        let registration = commands
            .register_owned(Arc::new(AgentConfigCommand {
                service: service.clone(),
                descriptor,
            }))
            .map_err(|error| CoreError::other(error.to_string()))?;
        context.effect(move || drop(registration));
    }
    context.effect(move || service.close());
    Ok(())
}
