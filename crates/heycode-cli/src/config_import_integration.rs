//! Human import review and real product authority observations.
//!
//! Source values remain inside the bounded parser/store owner. The slash command
//! only renders metadata, and all filesystem work runs off the async UI thread.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use heycode_agent::workspace_transition::{
    SERVICE_WORKSPACE_TRANSITION, WorkspaceTransitionHandle, WorkspaceTransitionService,
};
use heycode_agent::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandSource, CommandTiming, UiEvent,
};
use heycode_config::imports::{
    ImportCommitOutcome, ImportError, ImportProduct, ImportResourceKind, ImportSource, ImportStore,
    ImportTarget,
};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};
use heycode_extension_host::config_import::{
    ConfigImportDecision, ConfigImportHost, ConfigImportInventory, ConfigImportRequest,
    ConfigImportService, ConfirmedConfigImport, ImportHostSnapshot, ImportResourceKey,
    PinnedImportMount, PreparedConfigImport, SERVICE_CONFIG_IMPORT, SERVICE_IMPORT_MOUNT,
};
use heycode_trust::{ProjectInputKind, WorkspaceTrustService};
use tokio_util::sync::CancellationToken;

fn core(error: impl std::fmt::Display) -> CoreError {
    CoreError::other(error.to_string())
}
fn unavailable<T>(_: T) -> ImportError {
    ImportError::Unavailable
}

/// Compose the human import owner. `settings_paths` is user first, then the
/// admitted project file if present. All paths are supplied by the native host.
pub fn integration_plugin(
    heycode_home: PathBuf,
    settings_paths: Vec<PathBuf>,
    mount: Arc<PinnedImportMount>,
) -> Box<dyn Plugin> {
    struct Integration {
        home: PathBuf,
        settings: Vec<PathBuf>,
        mount: Arc<PinnedImportMount>,
    }
    impl Plugin for Integration {
        fn name(&self) -> &'static str {
            "config-import"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Command,
                    PluginContributionKind::Service,
                ],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_CONFIG_IMPORT]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[
                SERVICE_IMPORT_MOUNT,
                SERVICE_WORKSPACE_TRANSITION,
                heycode_trust::SERVICE_TRUST,
                heycode_settings::SERVICE_SETTINGS,
                heycode_skills::SERVICE_SKILLS,
                heycode_agent::SERVICE_COMMANDS,
                heycode_agent::SERVICE_SUBAGENTS,
                heycode_mcp::SERVICE_MCP,
            ]
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "import",
            )]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands = context
                .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| core("import command registry missing"))?;
            let host = Arc::new(ProductImportHost {
                home: self.home.clone(),
                settings_paths: self.settings.clone(),
                mount: self.mount.clone(),
                trust: context
                    .get::<WorkspaceTrustService>(heycode_trust::SERVICE_TRUST)
                    .ok_or_else(|| core("import trust owner missing"))?,
                workspace: context
                    .get::<WorkspaceTransitionHandle>(SERVICE_WORKSPACE_TRANSITION)
                    .ok_or_else(|| core("import workspace owner missing"))?
                    .0
                    .clone(),
                settings: context
                    .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                    .ok_or_else(|| core("import settings owner missing"))?,
                skills: context
                    .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
                    .ok_or_else(|| core("import skill owner missing"))?,
                commands: commands.clone(),
                subagents: context
                    .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
                    .ok_or_else(|| core("import subagent owner missing"))?,
                mcp: context
                    .get::<heycode_mcp::McpRegistry>(heycode_mcp::SERVICE_MCP)
                    .ok_or_else(|| core("import MCP registry missing"))?,
            });
            let service = Arc::new(ConfigImportService::new(
                ImportStore::new(&self.home).map_err(core)?,
                host.clone(),
                self.mount.generation(),
            ));
            // The core stores the exact Arc via a wrapper service value. The
            // controller and command use the same owner until disposal settles.
            context.provide(
                SERVICE_CONFIG_IMPORT,
                self.name(),
                ConfigImportHandle(service.clone()),
            )?;
            let controller = Arc::new(ImportController {
                service: service.clone(),
                host,
                mount: self.mount.clone(),
                state: Mutex::new(DialogState::default()),
                cancellation: Mutex::new(CancellationToken::new()),
            });
            let command = ImportCommand { controller, descriptor: CommandDescriptor::new(
                "import", "Review and import supported Codex, Gemini or Cursor resources",
                vec![CommandArgument::optional("action", "codex|gemini|cursor [--project] <absolute root>, select, confirm, cancel, recover, status").map_err(core)?],
                CommandTiming::Immediate, CommandSource::from_plugin(self.name()).map_err(core)?,
            ).map_err(core)? };
            commands
                .register_effect(context, Arc::new(command))
                .map_err(core)?;
            context.effect(move || service.close());
            Ok(())
        }
    }
    Box::new(Integration {
        home: heycode_home,
        settings: settings_paths,
        mount,
    })
}

/// The exact lifecycle-owned import service mounted by the CLI.
#[derive(Clone)]
pub struct ConfigImportHandle(pub Arc<ConfigImportService>);

struct ProductImportHost {
    home: PathBuf,
    settings_paths: Vec<PathBuf>,
    mount: Arc<PinnedImportMount>,
    trust: Arc<WorkspaceTrustService>,
    workspace: Arc<WorkspaceTransitionService>,
    settings: Arc<heycode_settings::SettingsService>,
    skills: Arc<heycode_skills::SkillSet>,
    commands: Arc<heycode_agent::CommandRegistry>,
    subagents: Arc<heycode_agent::SubagentRegistry>,
    mcp: Arc<heycode_mcp::McpRegistry>,
}

impl ConfigImportHost for ProductImportHost {
    fn admit_source(&self, request: &ConfigImportRequest) -> Result<(), ImportError> {
        self.snapshot(&request.target, &[])?;
        let source = ImportSource::open(&request.source_root)?;
        if matches!(request.target, ImportTarget::User) {
            let current = self.workspace.snapshot().map_err(unavailable)?;
            if source
                .canonical_root()
                .starts_with(self.workspace.original_root())
                || source.canonical_root().starts_with(&current.cwd)
            {
                return Err(ImportError::Authority);
            }
        }
        Ok(())
    }

    fn snapshot(
        &self,
        target: &ImportTarget,
        keys: &[ImportResourceKey],
    ) -> Result<ImportHostSnapshot, ImportError> {
        target.recheck()?;
        let trust = self.trust.snapshot().map_err(unavailable)?;
        let workspace = self.workspace.snapshot().map_err(unavailable)?;
        if let ImportTarget::Project { root, .. } = target {
            if root != trust.identity().canonical_root()
                || root != self.workspace.original_root()
                || !workspace.cwd.starts_with(root)
                || workspace.pending_recovery.is_some()
            {
                return Err(ImportError::Authority);
            }
            for input in [ProjectInputKind::Instructions, ProjectInputKind::Settings] {
                if !self.trust.access(input).map_err(unavailable)?.is_allowed() {
                    return Err(ImportError::Authority);
                }
            }
        }
        let mut observations = vec![
            format!("trust:{}:{:?}", trust.revision(), trust.decision()),
            serde_json::to_string(&workspace).map_err(unavailable)?,
        ];
        let mut conflicts = BTreeSet::new();
        let mcp = self.mcp.snapshot().map_err(unavailable)?;
        observations.push(format!("mcp:{}", mcp.revision()));
        for key in keys
            .iter()
            .filter(|key| key.kind == ImportResourceKind::Mcp)
        {
            if mcp
                .servers()
                .iter()
                .any(|server| server.definition().id().as_str() == key.name)
            {
                conflicts.insert(key.clone());
            }
        }
        // Native instruction files are separate from the settings watcher.
        // Capture their actual bytes so an editor change invalidates a preview.
        let mut instruction_paths = vec![self.home.join("AGENTS.md")];
        if self
            .trust
            .access(ProjectInputKind::Instructions)
            .map_err(unavailable)?
            .is_allowed()
        {
            instruction_paths.extend(
                heycode_prompt::instructions::WORKSPACE_INSTRUCTION_FILES
                    .iter()
                    .map(|name| workspace.cwd.join(name)),
            );
        }
        for (index, path) in instruction_paths.iter().enumerate() {
            observations.push(match read_native(path)? {
                Some(file) => file.fingerprint_for_host(),
                None => format!("instruction-{index}:absent"),
            });
        }
        let mounted = self.mount.keys();
        let settings = self.settings.describe().map_err(unavailable)?;
        for snapshot in &settings {
            observations.push(format!(
                "settings:{}:{}:{:?}",
                snapshot.namespace().as_str(),
                snapshot.revision(),
                snapshot.managed_locks()
            ));
            if snapshot.namespace().as_str() == "mcp-servers" {
                for key in keys
                    .iter()
                    .filter(|key| key.kind == ImportResourceKind::Mcp)
                {
                    if snapshot.managed().is_some_and(|value| {
                        value
                            .get("servers")
                            .and_then(|servers| servers.get(&key.name))
                            .is_some()
                    }) || snapshot.managed_locks().iter().any(|lock| {
                        lock.is_empty()
                            || lock == "/servers"
                            || lock.starts_with(&format!("/servers/{}/", key.name))
                            || lock == &format!("/servers/{}", key.name)
                    }) {
                        conflicts.insert(key.clone());
                    }
                }
            }
        }
        for (index, path) in self.settings_paths.iter().enumerate() {
            if index > 0
                && !self
                    .trust
                    .access(ProjectInputKind::Settings)
                    .map_err(unavailable)?
                    .is_allowed()
            {
                observations.push(format!("settings-file-{index}:denied"));
                continue;
            }
            let Some(file) = read_native(path)? else {
                observations.push(format!("settings-file-{index}:absent"));
                continue;
            };
            observations.push(file.fingerprint_for_host());
            let same_scope = matches!(target, ImportTarget::User) == (index == 0);
            if same_scope {
                let value: toml::Value = toml::from_str(file.text_for_parser()?)
                    .map_err(|_| ImportError::InvalidDocument)?;
                if let Some(servers) = value
                    .get("settings")
                    .and_then(|v| v.get("mcp-servers"))
                    .and_then(|v| v.get("servers"))
                {
                    for key in keys
                        .iter()
                        .filter(|key| key.kind == ImportResourceKind::Mcp)
                    {
                        if servers.get(&key.name).is_some() {
                            conflicts.insert(key.clone());
                        }
                    }
                }
            }
        }
        let scope = if matches!(target, ImportTarget::User) {
            "user"
        } else {
            "project"
        };
        let native_root = target
            .project_root()
            .map(|path| path.join(".heycode"))
            .unwrap_or_else(|| self.home.clone());
        let skill_records = self.skills.snapshot_records().map_err(unavailable)?;
        observations.push(format!(
            "skills:{}",
            self.skills.generation().map_err(unavailable)?
        ));
        let command_catalog = self.commands.catalog().map_err(unavailable)?;
        observations.extend(command_catalog.iter().map(|row| {
            format!(
                "command:{}:{}",
                row.descriptor.id(),
                row.descriptor.source().plugin()
            )
        }));
        observations.extend(
            self.subagents
                .presets()
                .iter()
                .map(|preset| format!("agent:{}:{:?}", preset.id(), preset.config())),
        );
        for key in keys {
            let already_imported = mounted
                .iter()
                .any(|(scope, imported)| scope == target && imported == key);
            match key.kind {
                ImportResourceKind::Agent => {
                    let native = read_native(
                        &native_root
                            .join("agents")
                            .join(format!("{}.json", key.name)),
                    )?;
                    if let Some(file) = native {
                        observations.push(file.fingerprint_for_host());
                        conflicts.insert(key.clone());
                    }
                    if !already_imported
                        && self
                            .subagents
                            .preset(&format!("{scope}-{}", key.name))
                            .is_some()
                    {
                        conflicts.insert(key.clone());
                    }
                    if self.subagents.preset(&key.name).is_some()
                        && self
                            .subagents
                            .preset(&format!("user-{}", key.name))
                            .is_none()
                        && self
                            .subagents
                            .preset(&format!("project-{}", key.name))
                            .is_none()
                    {
                        conflicts.insert(key.clone());
                    }
                }
                ImportResourceKind::Skill => {
                    let native =
                        read_native(&native_root.join("skills").join(&key.name).join("SKILL.md"))?;
                    if let Some(file) = native {
                        observations.push(file.fingerprint_for_host());
                        conflicts.insert(key.clone());
                    }
                    for row in skill_records
                        .iter()
                        .filter(|row| row.skill.name == key.name)
                    {
                        let same_scope = matches!(target, ImportTarget::User)
                            && row.source.scope() == heycode_skills::SkillSourceScope::User
                            || matches!(target, ImportTarget::Project { .. })
                                && row.source.scope() == heycode_skills::SkillSourceScope::Project;
                        if same_scope && !row.source.root().starts_with("import-generation-") {
                            conflicts.insert(key.clone());
                        }
                    }
                }
                ImportResourceKind::Command => {
                    if let Some(command) = self.commands.get(&key.name).map_err(unavailable)?
                        && command.descriptor().source().plugin() != "config-import-resources"
                    {
                        conflicts.insert(key.clone());
                    }
                }
                ImportResourceKind::Mcp | ImportResourceKind::Instructions => {}
            }
        }
        ImportHostSnapshot::from_private_observations(&observations, conflicts)
    }
}

// Bind the closest existing parent, then traverse every remaining component
// through ImportSource's no-follow, bounded reader. Missing files are observed
// explicitly; an unsafe file never becomes an apparently empty destination.
fn read_native(
    path: &Path,
) -> Result<Option<heycode_config::imports::ImportSourceFile>, ImportError> {
    if !path.is_absolute() {
        return Err(ImportError::UnsafePath);
    }
    let mut root = path.parent().ok_or(ImportError::UnsafePath)?;
    while !root.try_exists().map_err(unavailable)? {
        root = root.parent().ok_or(ImportError::UnsafePath)?;
    }
    let source = ImportSource::open(root)?;
    source.read(path.strip_prefix(root).map_err(unavailable)?)
}

#[derive(Default)]
struct DialogState {
    inventory: Option<ConfigImportInventory>,
    prepared: Option<PreparedConfigImport>,
    confirmed: Option<Arc<ConfirmedConfigImport>>,
}

struct ImportController {
    service: Arc<ConfigImportService>,
    host: Arc<ProductImportHost>,
    mount: Arc<PinnedImportMount>,
    state: Mutex<DialogState>,
    cancellation: Mutex<CancellationToken>,
}

impl ImportController {
    fn execute(&self, args: &str) -> Result<String, ImportError> {
        let (action, rest) = args
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((args.trim(), ""));
        if action == "cancel" {
            self.cancellation.lock().map_err(unavailable)?.cancel();
            if let Ok(mut state) = self.state.try_lock()
                && state.confirmed.is_none()
            {
                *state = DialogState::default();
            }
            return Ok("Import cancellation requested. An already published import remains committed; use /import recover if its outcome is pending.".to_owned());
        }
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(poison)) if action == "recover" => {
                poison.into_inner()
            }
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(ImportError::Unavailable),
            Err(std::sync::TryLockError::WouldBlock) => return Err(ImportError::Busy),
        };
        match action {
            "" | "help" => Ok(IMPORT_HELP.to_owned()),
            "status" => {
                let durable = ImportStore::new(&self.host.home)?.snapshot()?;
                Ok(format!(
                    "Active import generation: {}\nDurable import generation: {}\n{}\n{}",
                    self.mount.generation().revision(),
                    durable.revision(),
                    if durable.revision() != self.mount.generation().revision() {
                        "Recompose to activate the durable generation."
                    } else {
                        "The composed import generation is current."
                    },
                    self.mount.diagnostics().join("\n")
                ))
            }
            "codex" | "gemini" | "cursor" => {
                if state.confirmed.is_some() {
                    return Err(ImportError::Busy);
                }
                let mut path = rest.trim();
                if let Some(rest) = path.strip_prefix("--dry-run ") {
                    path = rest.trim();
                }
                let project = path.starts_with("--project ");
                if project {
                    path = path.trim_start_matches("--project ").trim();
                }
                if path.len() > 4096 || path.is_empty() || path.chars().any(char::is_control) {
                    return Err(ImportError::UnsafePath);
                }
                let target = if project {
                    ImportTarget::project(self.host.workspace.original_root())?
                } else {
                    ImportTarget::User
                };
                let inventory = self.service.discover(&ConfigImportRequest {
                    product: match action {
                        "codex" => ImportProduct::Codex,
                        "gemini" => ImportProduct::Gemini,
                        _ => ImportProduct::Cursor,
                    },
                    source_root: PathBuf::from(path),
                    target,
                })?;
                let mut lines = vec![format!("Import inventory {} (read only)", inventory.id())];
                lines.extend(inventory.items().iter().map(|row| {
                    format!(
                        "{} | {} | {} | {} | {}",
                        row.id,
                        kind_label(row.kind),
                        row.label,
                        status_label(row.status),
                        reason_label(row.reason)
                    )
                }));
                lines.push("Select exact rows with /import select item-1,item-2. Rename with item-3=new-name; keep with keep:item-4. Unlisted rows are omitted.".to_owned());
                *self.cancellation.lock().map_err(unavailable)? = CancellationToken::new();
                *state = DialogState {
                    inventory: Some(inventory),
                    ..DialogState::default()
                };
                Ok(lines.join("\n"))
            }
            "select" => {
                if state.confirmed.is_some() {
                    return Err(ImportError::Busy);
                }
                let decisions = parse_decisions(rest)?;
                let inventory = state.inventory.as_ref().ok_or(ImportError::Stale)?;
                if decisions
                    .iter()
                    .all(|d| matches!(d, ConfigImportDecision::KeepExisting { .. }))
                {
                    let mut kept = BTreeSet::new();
                    for decision in &decisions {
                        if let ConfigImportDecision::KeepExisting { item_id } = decision
                            && (!kept.insert(item_id)
                                || !inventory.items().iter().any(|row| &row.id == item_id))
                        {
                            return Err(ImportError::InvalidDocument);
                        }
                    }
                    state.prepared = None;
                    return Ok(
                        "All selected rows are kept. No import will be committed.".to_owned()
                    );
                }
                let prepared = self.service.prepare(inventory, &decisions)?;
                let view = prepared.preview();
                let mut lines = vec![format!(
                    "Reviewed {} import; baseline generation {}; {} identical resources already present.",
                    view.scope, view.baseline_revision, view.duplicates
                )];
                lines.extend(view.actions.iter().map(|row| {
                    format!(
                        "{} -> {} {}",
                        row.item_id,
                        kind_label(Some(row.kind)),
                        row.name
                    )
                }));
                let kept = decisions
                    .iter()
                    .filter_map(|decision| match decision {
                        ConfigImportDecision::KeepExisting { item_id } => Some(item_id.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !kept.is_empty() {
                    lines.push(format!(
                        "Keep current configuration for: {}",
                        kept.join(", ")
                    ));
                }
                lines.push(
                    "MCP servers stay disabled. Activation requires a fresh composition."
                        .to_owned(),
                );
                lines.push(format!(
                    "Confirm this exact selection with /import confirm {}",
                    view.digest
                ));
                state.prepared = Some(prepared);
                Ok(lines.join("\n"))
            }
            "confirm" => {
                if state.confirmed.is_some() {
                    return Err(ImportError::Busy);
                }
                let prepared = state
                    .prepared
                    .take()
                    .ok_or(ImportError::ConfirmationRequired)?;
                let confirmed = Arc::new(self.service.confirm_human(prepared, rest.trim())?);
                state.confirmed = Some(confirmed.clone());
                let cancel = self.cancellation.lock().map_err(unavailable)?.clone();
                match self.service.commit(&confirmed, &cancel) {
                    Ok(ImportCommitOutcome::Committed(receipt)) => {
                        *state = DialogState::default();
                        Ok(format!("Imported {} resources in generation {}. Recompose to activate; this session retains generation {}.", receipt.added, receipt.revision, self.mount.generation().revision()))
                    }
                    Ok(ImportCommitOutcome::Unchanged { revision }) => {
                        *state = DialogState::default();
                        Ok(format!("All selected resources already exist unchanged in generation {revision}. No files were written."))
                    }
                    Ok(ImportCommitOutcome::CancelledBeforePublication) => {
                        *state = DialogState::default();
                        Ok("Import cancelled before publication. The active generation is unchanged.".to_owned())
                    }
                    Ok(ImportCommitOutcome::RecoveryRequired { .. }) => Ok("Import publication needs reconciliation. Use /import recover; do not start another import.".to_owned()),
                    Err(error) => {
                        // A pre-publication failure is known here; uncertain
                        // publication is always the explicit outcome above.
                        state.confirmed = None;
                        Err(error)
                    }
                }
            }
            "recover" => {
                let confirmed = state.confirmed.as_ref().ok_or(ImportError::Stale)?;
                let result = self.service.reconcile(confirmed)?;
                *state = DialogState::default();
                self.state.clear_poison();
                Ok(match result {
                    Some(receipt) => format!("Confirmed committed import generation {} ({} resources). Recompose to activate it.", receipt.revision, receipt.added),
                    None => "Reconciliation confirmed no publication. Prepare a new inventory before retrying.".to_owned(),
                })
            }
            _ => Ok(IMPORT_HELP.to_owned()),
        }
    }
}

const IMPORT_HELP: &str = "/import codex|gemini|cursor [--dry-run] [--project] <absolute source root>\nUser source: foreign home directory. Project source: exact current workspace root.\n/import select item-1,item-2=new-name,keep:item-3\n/import confirm <reviewed digest>\n/import cancel | recover | status\nOnly selected supported resources are imported. Unsupported and excluded rows remain visible in the inventory.";

fn kind_label(kind: Option<ImportResourceKind>) -> &'static str {
    match kind {
        Some(ImportResourceKind::Agent) => "Agent",
        Some(ImportResourceKind::Skill) => "Skill",
        Some(ImportResourceKind::Command) => "Command",
        Some(ImportResourceKind::Instructions) => "Instructions",
        Some(ImportResourceKind::Mcp) => "MCP server",
        None => "Setting",
    }
}

fn status_label(status: heycode_extension_host::config_import::ImportItemStatus) -> &'static str {
    use heycode_extension_host::config_import::ImportItemStatus as Status;
    match status {
        Status::Ready => "Ready",
        Status::Conflict => "Conflict",
        Status::Duplicate => "Already imported",
        Status::NeedsRename => "Rename needed",
        Status::NeedsBinding => "Manual setup",
        Status::Unsupported => "Unsupported",
        Status::Excluded => "Excluded",
    }
}

fn reason_label(reason: heycode_extension_host::config_import::ImportItemReason) -> &'static str {
    use heycode_extension_host::config_import::ImportItemReason as Reason;
    match reason {
        Reason::Supported => "Supported",
        Reason::McpInstalledDisabled => "Starts disabled",
        Reason::InvalidDestinationName => "Choose a valid native name",
        Reason::ExistingDestination => "A resource already uses this name",
        Reason::CredentialOrInterpolation => "Credentials or substitutions need manual setup",
        Reason::ProviderBinding => "Choose a native provider and model separately",
        Reason::TransportBinding => "Transport needs manual setup",
        Reason::ScopeBinding => "Set up this project server manually",
        Reason::UnsupportedFields => "Contains unsupported settings",
        Reason::ActivationSemantics => "Activation behavior needs manual setup",
        Reason::ExternalDependency => "Includes, scripts or bundled files need manual setup",
        Reason::InvalidDocument => "Source format could not be validated",
        Reason::AuthorityExcluded => "Trust and permission grants are not imported",
    }
}

fn parse_decisions(args: &str) -> Result<Vec<ConfigImportDecision>, ImportError> {
    if args.len() > 32 * 1024 {
        return Err(ImportError::Limit);
    }
    let mut decisions = Vec::new();
    for token in args.split(',').map(str::trim) {
        if token.is_empty() {
            return Err(ImportError::InvalidDocument);
        }
        let decision = if let Some(id) = token.strip_prefix("keep:") {
            ConfigImportDecision::KeepExisting {
                item_id: id.to_owned(),
            }
        } else if let Some((id, name)) = token.split_once('=') {
            ConfigImportDecision::Rename {
                item_id: id.to_owned(),
                name: name.to_owned(),
            }
        } else {
            ConfigImportDecision::Add {
                item_id: token.to_owned(),
            }
        };
        decisions.push(decision);
    }
    if decisions.len() > heycode_config::imports::MAX_RESOURCES {
        return Err(ImportError::Limit);
    }
    Ok(decisions)
}

struct ImportCommand {
    controller: Arc<ImportController>,
    descriptor: CommandDescriptor,
}
#[async_trait::async_trait]
impl Command for ImportCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let controller = self.controller.clone();
        let args = args.to_owned();
        let result = tokio::task::spawn_blocking(move || controller.execute(&args)).await;
        let text = match result {
            Ok(Ok(text)) => text,
            Ok(Err(error)) => error.to_string(),
            Err(_) => "Import worker did not settle normally. Use /import recover before starting another import.".to_owned(),
        };
        agent.ui().emit(UiEvent::Info { text });
        Ok(())
    }
}
