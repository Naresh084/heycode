use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandRegistry, CommandSource,
    CommandTiming, SubagentPreset,
};
use heycode_config::imports::ImportGeneration;
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};
use heycode_settings::{SettingsDocuments, SettingsNamespace};
use heycode_settings_file::PinnedSettingsOverlay;
use heycode_skills::{Skill, SkillSet, SkillSourceScope};
use heycode_trust::{ProjectInputKind, WorkspaceTrustService};
use serde_json::Value;

use super::*;

/// The exact resource generation mounted in a composed product world.
pub const SERVICE_IMPORT_MOUNT: ServiceKey = ServiceKey::new("config-import-mount");

/// Frozen, scope-filtered resources shared by settings, declarations and late mounts.
/// Construction reads no source configuration and performs no activation.
#[derive(Clone)]
pub struct PinnedImportMount {
    generation: Arc<ImportGeneration>,
    entries: Vec<heycode_config::imports::ImportedEntry>,
    trust: WorkspaceTrustService,
    trust_revision: u64,
    workspace: ImportTarget,
    live: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for PinnedImportMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedImportMount")
            .field("revision", &self.generation.revision())
            .field("resources", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl PinnedImportMount {
    /// Bind the one already-read generation to the current real workspace policy.
    /// Other projects stay inactive; a replaced same-path project is refused.
    pub fn new(
        generation: Arc<ImportGeneration>,
        trust: WorkspaceTrustService,
    ) -> Result<Self, ImportError> {
        let snapshot = trust.snapshot().map_err(|_| ImportError::Authority)?;
        let workspace = ImportTarget::project(snapshot.identity().canonical_root())?;
        let mut entries = Vec::new();
        for entry in generation.entries() {
            match entry.target() {
                ImportTarget::User => {}
                target @ ImportTarget::Project { root, .. } => {
                    if Some(root.as_path()) != workspace.project_root() {
                        continue;
                    }
                    if target != &workspace {
                        return Err(ImportError::Stale);
                    }
                    let input = if entry.resource().kind() == ImportResourceKind::Mcp {
                        ProjectInputKind::Settings
                    } else {
                        ProjectInputKind::Instructions
                    };
                    if !trust
                        .access(input)
                        .map_err(|_| ImportError::Authority)?
                        .is_allowed()
                    {
                        continue;
                    }
                }
            }
            validate_payload(entry.resource())?;
            entries.push(entry.clone());
        }
        Ok(Self {
            generation,
            entries,
            trust,
            trust_revision: snapshot.revision(),
            workspace,
            live: Arc::new(AtomicBool::new(true)),
            diagnostics: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Exact store snapshot all resource consumers share.
    pub fn generation(&self) -> Arc<ImportGeneration> {
        self.generation.clone()
    }

    /// Whether project-bound resources require the composition's workspace to stay fixed.
    pub fn has_project_resources(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry.target(), ImportTarget::Project { .. }))
    }

    /// Readable mounted resource keys, excluding other projects or withheld project policy.
    pub fn keys(&self) -> Vec<(ImportTarget, ImportResourceKey)> {
        self.entries
            .iter()
            .map(|entry| {
                (
                    entry.target().clone(),
                    ImportResourceKey {
                        kind: entry.resource().kind(),
                        name: entry.resource().name().to_owned(),
                    },
                )
            })
            .collect()
    }

    /// Fixed safe diagnostics for native-over-import precedence decisions.
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_else(|_| vec!["import diagnostics unavailable".to_owned()])
    }

    /// Resolve strict imported agents before the existing declaration owner composes.
    /// Pass these exact presets to its imported-agent constructor, not a second registry.
    pub fn agent_presets(&self) -> Result<Vec<SubagentPreset>, ImportError> {
        self.recheck()?;
        self.entries
            .iter()
            .filter(|entry| entry.resource().kind() == ImportResourceKind::Agent)
            .map(|entry| {
                crate::user_declarations::preset_from_file(
                    entry.resource().payload_for_activation(),
                    scope_name(entry.target()),
                    entry.resource().name(),
                )
                .map_err(|_| ImportError::InvalidDocument)
            })
            .collect()
    }

    fn recheck(&self) -> Result<(), ImportError> {
        if !self.live.load(Ordering::SeqCst)
            || self
                .trust
                .snapshot()
                .map_err(|_| ImportError::Authority)?
                .revision()
                != self.trust_revision
        {
            return Err(ImportError::Stale);
        }
        self.workspace.recheck()
    }

    fn note_native_precedence(&self, kind: ImportResourceKind, name: &str) {
        if let Ok(mut rows) = self.diagnostics.lock() {
            let row =
                format!("native {kind:?} `{name}` takes precedence over its imported definition");
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }

    fn mcp_layer(
        &self,
        target: &ImportTarget,
    ) -> Result<serde_json::Map<String, Value>, ImportError> {
        let mut servers = serde_json::Map::new();
        for entry in self.entries.iter().filter(|entry| {
            entry.target() == target && entry.resource().kind() == ImportResourceKind::Mcp
        }) {
            let payload: ImportedMcpDocument =
                parse_private(entry.resource().payload_for_activation())?;
            let row = match payload {
                ImportedMcpDocument::Stdio { command, args } => {
                    serde_json::json!({"transport":"stdio", "target":command, "args":args, "enabled":false})
                }
                ImportedMcpDocument::StreamableHttp { url } => {
                    serde_json::json!({"transport":"streamable-http", "target":url, "enabled":false})
                }
            };
            servers.insert(entry.resource().name().to_owned(), row);
        }
        Ok(servers)
    }
}

impl PinnedSettingsOverlay for PinnedImportMount {
    fn merge(&self, mut native: SettingsDocuments) -> Result<SettingsDocuments, String> {
        self.recheck().map_err(|error| error.to_string())?;
        let namespace = SettingsNamespace::new("mcp-servers")
            .map_err(|_| ImportError::InvalidDocument.to_string())?;
        for target in [&ImportTarget::User, &self.workspace] {
            let imported = self.mcp_layer(target).map_err(|error| error.to_string())?;
            if imported.is_empty() {
                continue;
            }
            let is_user = matches!(target, ImportTarget::User);
            let higher_native_keys = [
                is_user
                    .then(|| native.project_section(&namespace))
                    .flatten(),
                native.managed_section(&namespace),
            ]
            .into_iter()
            .flatten()
            .filter_map(|layer| layer.get("servers").and_then(Value::as_object))
            .flat_map(|servers| servers.keys().cloned())
            .collect::<std::collections::BTreeSet<_>>();
            let original = if is_user {
                native.user_section(&namespace)
            } else {
                native.project_section(&namespace)
            };
            let mut section = original.cloned().unwrap_or_else(|| serde_json::json!({}));
            let section_map = section
                .as_object_mut()
                .ok_or_else(|| ImportError::InvalidDocument.to_string())?;
            let servers = section_map
                .entry("servers")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or_else(|| ImportError::InvalidDocument.to_string())?;
            for (name, row) in imported {
                // A server is an indivisible definition. In particular, a native
                // enabled=true fragment must not enable an inherited command.
                if servers.contains_key(&name) || higher_native_keys.contains(&name) {
                    self.note_native_precedence(ImportResourceKind::Mcp, &name);
                } else {
                    servers.insert(name, row);
                }
            }
            if is_user {
                native.set_user(namespace.clone(), section)
            } else {
                native.set_project(namespace.clone(), section)
            }
            .map_err(|_| ImportError::InvalidDocument.to_string())?;
        }
        Ok(native)
    }
}

/// Register frozen skills, prompt commands and scoped instruction sections from
/// the same generation used by settings and the existing agent declaration owner.
pub fn imported_resources_plugin(mount: Arc<PinnedImportMount>) -> Box<dyn Plugin> {
    struct ImportedResources(Arc<PinnedImportMount>);
    impl Plugin for ImportedResources {
        fn name(&self) -> &'static str {
            "config-import-resources"
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Service,
                    PluginContributionKind::Command,
                    PluginContributionKind::PromptSection,
                ],
            )
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_IMPORT_MOUNT]
        }
        fn inject(&self) -> &'static [ServiceKey] {
            &[
                heycode_skills::SERVICE_SKILLS,
                heycode_prompt::SERVICE_PROMPT,
                heycode_agent::SERVICE_COMMANDS,
                crate::SERVICE_AGENT_DECLARATIONS,
            ]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let mount = &self.0;
            mount.recheck().map_err(core)?;
            let skills = context
                .get::<SkillSet>(heycode_skills::SERVICE_SKILLS)
                .ok_or_else(|| core(ImportError::Unavailable))?;
            let commands = context
                .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| core(ImportError::Unavailable))?;
            let prompt = context
                .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
                .ok_or_else(|| core(ImportError::Unavailable))?;
            // Prevalidate every payload before any registrations become visible.
            for entry in &mount.entries {
                validate_payload(entry.resource()).map_err(core)?;
            }
            let mut instructions = BTreeMap::<&str, Vec<(ImportTarget, String, String)>>::new();
            for entry in &mount.entries {
                let resource = entry.resource();
                match resource.kind() {
                    ImportResourceKind::Agent | ImportResourceKind::Mcp => {} // Existing owners mount these.
                    ImportResourceKind::Skill => {
                        let document: ImportedSkillDocument =
                            parse_private(resource.payload_for_activation()).map_err(core)?;
                        let scope = if matches!(entry.target(), ImportTarget::User) {
                            SkillSourceScope::User
                        } else {
                            SkillSourceScope::Project
                        };
                        let registered = skills
                            .register_imported_owned(
                                Skill {
                                    name: resource.name().to_owned(),
                                    description: document.description,
                                    disable_model_invocation: document.disable_model_invocation,
                                    body: document.body,
                                },
                                scope,
                                &format!("import-generation-{}", mount.generation.revision()),
                            )
                            .map_err(core)?;
                        context
                            .contribute(heycode_core::ContributionKind::Skill, resource.name())?;
                        context.effect(move || drop(registered));
                    }
                    ImportResourceKind::Command => {
                        if matches!(entry.target(), ImportTarget::User)
                            && mount.entries.iter().any(|candidate| {
                                matches!(candidate.target(), ImportTarget::Project { .. })
                                    && candidate.resource().kind() == ImportResourceKind::Command
                                    && candidate.resource().name() == resource.name()
                            })
                        {
                            continue;
                        }
                        if commands.get(resource.name()).map_err(core)?.is_some() {
                            mount.note_native_precedence(resource.kind(), resource.name());
                            continue;
                        }
                        let document: ImportedCommandDocument =
                            parse_private(resource.payload_for_activation()).map_err(core)?;
                        let descriptor = CommandDescriptor::new(
                            resource.name(),
                            document.description.clone(),
                            vec![
                                CommandArgument::optional(
                                    "args",
                                    "Arguments for the imported prompt",
                                )
                                .map_err(core)?
                                .variadic(),
                            ],
                            CommandTiming::ModelScheduling,
                            CommandSource::from_plugin("config-import-resources").map_err(core)?,
                        )
                        .map_err(core)?;
                        let registered = commands
                            .register_owned(Arc::new(ImportedCommand {
                                descriptor,
                                document,
                                target: entry.target().clone(),
                                live: mount.live.clone(),
                            }))
                            .map_err(core)?;
                        context
                            .contribute(heycode_core::ContributionKind::Command, resource.name())?;
                        context.effect(move || drop(registered));
                    }
                    ImportResourceKind::Instructions => {
                        let document: ImportedInstructionDocument =
                            parse_private(resource.payload_for_activation()).map_err(core)?;
                        instructions
                            .entry(scope_name(entry.target()))
                            .or_default()
                            .push((
                                entry.target().clone(),
                                resource.name().to_owned(),
                                document.text,
                            ));
                    }
                }
            }
            for (scope, rows) in instructions {
                let name = if scope == "user" {
                    "imported-user-instructions"
                } else {
                    "imported-project-instructions"
                };
                let live = mount.live.clone();
                prompt
                    .section_shared(
                        name,
                        if scope == "user" { 47 } else { 49 },
                        move |context| {
                            if !live.load(Ordering::SeqCst) {
                                return String::new();
                            }
                            rows.iter()
                                .filter(|(target, _, _)| within_target(target, &context.cwd))
                                .map(|(_, label, text)| {
                                    format!("# Imported {scope} instructions ({label})\n\n{text}")
                                })
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        },
                    )
                    .map_err(core)?;
                context.contribute(heycode_core::ContributionKind::PromptSection, name)?;
            }
            context.provide(SERVICE_IMPORT_MOUNT, self.name(), (**mount).clone())?;
            let live = mount.live.clone();
            context.effect(move || live.store(false, Ordering::SeqCst));
            Ok(())
        }
    }
    Box::new(ImportedResources(mount))
}

struct ImportedCommand {
    descriptor: CommandDescriptor,
    document: ImportedCommandDocument,
    target: ImportTarget,
    live: Arc<AtomicBool>,
}

#[async_trait]
impl Command for ImportedCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.live.load(Ordering::SeqCst) && within_target(&self.target, &agent.cwd()),
            "Imported command is not active for this workspace; recompose before using it"
        );
        anyhow::ensure!(
            args.len() <= heycode_config::imports::MAX_FILE_BYTES,
            "Imported command arguments exceed the input limit"
        );
        let prompt = render_command(&self.document, self.descriptor.id(), args)?;
        let _report = agent.send(&prompt).await?;
        Ok(())
    }
}

fn render_command(
    document: &ImportedCommandDocument,
    name: &str,
    args: &str,
) -> Result<String, ImportError> {
    let count = document.prompt.matches("{{args}}").count();
    let length = if count > 0 {
        document
            .prompt
            .len()
            .checked_sub(count * "{{args}}".len())
            .and_then(|base| {
                args.len()
                    .checked_mul(count)
                    .and_then(|added| base.checked_add(added))
            })
    } else if args.is_empty() {
        Some(document.prompt.len())
    } else {
        document
            .prompt
            .len()
            .checked_add(args.len())
            .and_then(|length| length.checked_add(name.len() + 4))
    };
    if length.is_none_or(|length| length > heycode_config::imports::MAX_FILE_BYTES) {
        return Err(ImportError::Limit);
    }
    Ok(match document.argument_mode {
        ImportedArgumentMode::Gemini => {
            if document.prompt.contains("{{args}}") {
                document.prompt.replace("{{args}}", args)
            } else if args.is_empty() {
                document.prompt.clone()
            } else {
                format!("{}\n\n/{name} {args}", document.prompt)
            }
        }
    })
}

fn within_target(target: &ImportTarget, cwd: &Path) -> bool {
    match target {
        ImportTarget::User => true,
        ImportTarget::Project { root, .. } => cwd.starts_with(root) && target.recheck().is_ok(),
    }
}

fn scope_name(target: &ImportTarget) -> &'static str {
    if matches!(target, ImportTarget::User) {
        "user"
    } else {
        "project"
    }
}

fn core(error: impl std::fmt::Display) -> CoreError {
    CoreError::other(error.to_string())
}

fn validate_payload(resource: &ImportResource) -> Result<(), ImportError> {
    match resource.kind() {
        ImportResourceKind::Agent => {
            if resource.product() != ImportProduct::Codex {
                return Err(ImportError::InvalidDocument);
            }
            let document: crate::AgentDocument = parse_private(resource.payload_for_activation())?;
            let expected = heycode_agent::SubagentConfig {
                permissions: document.config.permissions,
                ..Default::default()
            };
            if document.provider.is_some()
                || !matches!(document.mode, crate::AgentModeDocument::OneShot)
                || document.config != expected
                || !matches!(
                    document.config.permissions,
                    heycode_agent::ChildPermissions::Inherit
                        | heycode_agent::ChildPermissions::ReadOnly
                )
                || parser::suspected_secret(&document.instructions)
            {
                return Err(ImportError::InvalidDocument);
            }
            crate::user_declarations::validate_agent_document(resource.payload_for_activation())
                .map_err(|_| ImportError::InvalidDocument)?;
        }
        ImportResourceKind::Skill => {
            if resource.product() != ImportProduct::Codex {
                return Err(ImportError::InvalidDocument);
            }
            let document: ImportedSkillDocument = parse_private(resource.payload_for_activation())?;
            if !parser::safe_description(&document.description)
                || !parser::valid_text(&document.body, heycode_config::imports::MAX_FILE_BYTES)
                || parser::suspected_secret(&document.body)
            {
                return Err(ImportError::InvalidDocument);
            }
        }
        ImportResourceKind::Command => {
            if resource.product() != ImportProduct::Gemini {
                return Err(ImportError::InvalidDocument);
            }
            let document: ImportedCommandDocument =
                parse_private(resource.payload_for_activation())?;
            if !parser::safe_description(&document.description)
                || !parser::valid_text(&document.prompt, heycode_config::imports::MAX_FILE_BYTES)
                || document.prompt.contains("!{")
                || document.prompt.contains("@{")
                || parser::suspected_secret(&document.prompt)
            {
                return Err(ImportError::InvalidDocument);
            }
        }
        ImportResourceKind::Instructions => {
            let document: ImportedInstructionDocument =
                parse_private(resource.payload_for_activation())?;
            if !parser::valid_text(
                &document.text,
                heycode_prompt::instructions::MAX_INSTRUCTION_BYTES,
            ) || parser::suspected_secret(&document.text)
            {
                return Err(ImportError::InvalidDocument);
            }
        }
        ImportResourceKind::Mcp => {
            let document: ImportedMcpDocument = parse_private(resource.payload_for_activation())?;
            match document {
                ImportedMcpDocument::Stdio { command, args } => {
                    if parser::suspected_secret(&command)
                        || args
                            .iter()
                            .any(|arg| parser::suspected_secret(arg) || arg.contains('$'))
                    {
                        return Err(ImportError::InvalidDocument);
                    }
                    heycode_mcp::management::StoredServer::new(
                        resource.name(),
                        heycode_mcp::McpTransportKind::Stdio,
                        command,
                    )
                    .and_then(|server| server.with_stdio_launch(args, BTreeMap::new()))
                    .map_err(|_| ImportError::InvalidDocument)?;
                }
                ImportedMcpDocument::StreamableHttp { url } => {
                    heycode_mcp::McpStreamableHttpTransport::new(url, BTreeMap::new())
                        .map_err(|_| ImportError::InvalidDocument)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn gemini_arguments_preserve_raw_substitution_and_full_invocation() {
        let mut document = ImportedCommandDocument {
            description: "Example".to_owned(),
            prompt: "Review {{args}} twice: {{args}}".to_owned(),
            argument_mode: ImportedArgumentMode::Gemini,
        };
        assert_eq!(
            render_command(&document, "review", "\"two words\"  tail").unwrap(),
            "Review \"two words\"  tail twice: \"two words\"  tail"
        );
        document.prompt = "Instructions".to_owned();
        assert_eq!(
            render_command(&document, "renamed", "one  two").unwrap(),
            "Instructions\n\n/renamed one  two"
        );
        assert_eq!(
            render_command(&document, "renamed", "").unwrap(),
            "Instructions"
        );
        document.prompt = "{{args}}{{args}}".to_owned();
        assert_eq!(
            render_command(
                &document,
                "renamed",
                &"x".repeat(heycode_config::imports::MAX_FILE_BYTES)
            ),
            Err(ImportError::Limit)
        );
    }
}
