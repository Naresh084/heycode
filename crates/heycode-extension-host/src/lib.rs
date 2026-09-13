//! Product registry adapters for declarative extension packages.
//!
//! `heycode-extensions` deliberately stops at a host-neutral, immutable document
//! bridge. This crate is the product host above that boundary: it validates the
//! six PL03 document shapes against live registries, activates PL04 bundled
//! MCP definitions through the ordinary effect-owned MCP transport plugin and
//! provides PL09's two-phase product proxy-generation transaction.
//! Nothing here discovers ambient source files. Inputs are verified PL02 cache
//! receipts, and every live row is withdrawn by the owning Context in LIFO
//! order.

mod code_plugin_adapters;
mod code_plugin_host;
mod code_plugin_process;
mod installed_code;
mod wasi_component_engine;

pub use code_plugin_host::{
    CodePluginInvocation, CodePluginProductAdapter, CodePluginProductRegistration,
    ProductCodePluginHost, ProductCodePluginHostError,
};
pub use code_plugin_process::HeycodeExecCodePluginLauncher;
pub use installed_code::{
    InstalledCodePluginAuthority, InstalledCodePluginAuthorityProvider, InstalledCodePluginError,
    InstalledCodePluginResources, ManagedCodePluginAuthorityGeneration,
    ManagedCodePluginAuthorityRule, ManagedCodePluginResourceSpec, ManagedCodePluginSessionPolicy,
    ManagedInstalledCodePluginAuthorityProvider, ManagedWasiNetworkEndpointSpec,
    ManagedWasiPreopenSpec, installed_product_extensions_plugin_from_root_with_code,
    installed_product_extensions_plugin_from_root_with_code_provider,
    installed_product_extensions_plugin_with_code,
    installed_product_extensions_plugin_with_imported_agents,
    installed_product_extensions_plugin_with_user_declarations,
};
pub use wasi_component_engine::WasmtimeWasiComponentEngine;

/// Confirmed competitor import planning and pinned native activation.
pub mod config_import;
pub mod user_declarations;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandDescriptor, CommandRegistration, CommandSource, CommandTiming,
    SubagentContinuation, SubagentPreset, SubagentPresetRegistration, SubagentProviderId,
    SubagentRegistry, SubagentSeed,
};
use heycode_core::{
    Context, ContributionKind as CoreContributionKind, CoreError, Plugin, PluginContributionKind,
    PluginContributionSpec, PluginDescriptor, ServiceKey,
};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialsService,
};
use heycode_extensions::{
    ContributionKind, DeclarativeContribution, DeclarativeContributionHost,
    DeclarativeContributionRegistration, DeclarativePackage, HostActivationFailure,
    ManifestValidator, PluginInstallCache, PluginPermission, declarative_activation_plugin,
};
use heycode_hooks::{Hook, HookAction, HookEvent, HookPhase, HookRegistration, HookService};
use heycode_llm::{
    ChunkStream, InferenceAdapter, LlmError, ModelDescriptor, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig, Provider, ProviderDescriptor, ProviderInfo, ProviderProtocol,
    ProviderRegistration, ProviderRegistry, RouteCredential,
};
use heycode_mcp::{
    McpApprovalMode, McpArgument, McpDefinitionScope, McpEnvironmentValue, McpExposurePolicy,
    McpReconnectPolicy, McpServerDefinition, McpServerSpec, McpStdioTransport, McpTimeouts,
    McpToolPolicy, McpTransportDefinition,
};
use heycode_skills::{Skill, SkillRegistration, SkillSet};
use heycode_ui::theme::{Rgb, Theme};
use heycode_ui::{ThemeRegistration, UiRegistry};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// Static product plugin id for installed declarative extensions.
pub const PRODUCT_EXTENSIONS_PLUGIN_ID: &str = "product-extensions";

pub(crate) const MAX_TEXT_BYTES: usize = 1024 * 1024;
const PRODUCT_SERVICES: &[ServiceKey] = &[
    heycode_skills::SERVICE_SKILLS,
    heycode_agent::SERVICE_COMMANDS,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_hooks::SERVICE_HOOKS,
    heycode_ui::SERVICE_UI,
    heycode_llm::SERVICE_PROVIDERS,
    heycode_http::SERVICE_HTTP,
    heycode_credentials::SERVICE_CREDENTIALS,
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
    heycode_mcp::SERVICE_MCP,
];
const INSTALLED_PRODUCT_SERVICES: &[ServiceKey] = &[
    heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
    heycode_skills::SERVICE_SKILLS,
    heycode_agent::SERVICE_COMMANDS,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_hooks::SERVICE_HOOKS,
    heycode_ui::SERVICE_UI,
    heycode_llm::SERVICE_PROVIDERS,
    heycode_http::SERVICE_HTTP,
    heycode_credentials::SERVICE_CREDENTIALS,
    heycode_tools::SERVICE_TOOLS,
    heycode_exec::SERVICE_SUBPROCESS,
    heycode_mcp::SERVICE_MCP,
];
const PRODUCT_FAMILIES: &[PluginContributionKind] = &[
    PluginContributionKind::Service,
    PluginContributionKind::PromptSection,
    PluginContributionKind::Command,
    PluginContributionKind::Provider,
    PluginContributionKind::Waterfall,
    PluginContributionKind::UserInterface,
    PluginContributionKind::ExternalProcess,
    PluginContributionKind::Tool,
];

/// Stable product-host activation failures. No variant contains document
/// bytes, credential values, process arguments, or package paths.
#[derive(Debug, thiserror::Error)]
pub enum ProductExtensionError {
    /// A PL03 document failed its strict domain schema.
    #[error("declarative extension definition is invalid")]
    InvalidDefinition,
    /// The package omitted authority required for this contribution.
    #[error("declarative extension permission is missing")]
    MissingPermission,
    /// An enabled package could not be resolved from the verified cache.
    #[error("enabled declarative extension package is unavailable")]
    PackageUnavailable,
    /// A live registry or lifecycle service is unavailable.
    #[error("declarative extension host is unavailable")]
    HostUnavailable,
    /// An enabled code package requires the explicit installed-code authority path.
    #[error("enabled code extension requires explicit runtime authority")]
    CodeAuthorityRequired,
    /// A contribution collided with an existing live row.
    #[error("declarative extension contribution is already registered")]
    Duplicate,
}

/// Activate an exact already-verified package generation in a real product
/// composition.
///
/// This constructor is used by managed-policy Consumers that already hold the
/// exact package generation. For the normal installed-state path use
/// [`installed_product_extensions_plugin`].
///
/// # Errors
/// Duplicate package/contribution claims fail before a plugin is returned.
pub fn product_extensions_plugin(
    packages: Vec<DeclarativePackage>,
) -> Result<Box<dyn Plugin>, ProductExtensionError> {
    ProductExtensionsPlugin::new(packages, false).map(|plugin| Box::new(plugin) as Box<dyn Plugin>)
}

/// Activate every enabled lifecycle row from one verified PL02 cache.
///
/// Resolution rehashes each exact active version before any contribution is
/// published. Rows are discovered only during apply, so their inventory is
/// contributed dynamically inside the same composition transaction.
#[must_use]
pub fn installed_product_extensions_plugin(cache: PluginInstallCache) -> Box<dyn Plugin> {
    Box::new(InstalledProductExtensionsPlugin { cache })
}

/// Lazily open the verified PL02 cache when the plugin applies.
///
/// The composition root uses this form because the credentials/home owner
/// directory is itself established by earlier plugins; factory inspection must
/// remain side-effect free and cannot assume that directory already exists.
#[must_use]
pub fn installed_product_extensions_plugin_from_root(
    root: PathBuf,
    validator: ManifestValidator,
) -> Box<dyn Plugin> {
    Box::new(LazyInstalledProductExtensionsPlugin { root, validator })
}

struct LazyInstalledProductExtensionsPlugin {
    root: PathBuf,
    validator: ManifestValidator,
}

impl Plugin for LazyInstalledProductExtensionsPlugin {
    fn name(&self) -> &'static str {
        PRODUCT_EXTENSIONS_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            PRODUCT_EXTENSIONS_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            PRODUCT_FAMILIES,
        )
    }

    fn inject(&self) -> &'static [ServiceKey] {
        INSTALLED_PRODUCT_SERVICES
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let cache = PluginInstallCache::open(&self.root, self.validator.clone())
            .map_err(|_| CoreError::other(ProductExtensionError::PackageUnavailable.to_string()))?;
        InstalledProductExtensionsPlugin { cache }.apply(context)
    }
}

struct InstalledProductExtensionsPlugin {
    cache: PluginInstallCache,
}

impl Plugin for InstalledProductExtensionsPlugin {
    fn name(&self) -> &'static str {
        PRODUCT_EXTENSIONS_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            PRODUCT_EXTENSIONS_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            PRODUCT_FAMILIES,
        )
    }

    fn inject(&self) -> &'static [ServiceKey] {
        INSTALLED_PRODUCT_SERVICES
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let lifecycle = context
            .get::<heycode_extensions::lifecycle::PluginLifecycle>(
                heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
            )
            .ok_or_else(|| CoreError::other(ProductExtensionError::HostUnavailable.to_string()))?;
        let states = lifecycle
            .list()
            .map_err(|_| CoreError::other(ProductExtensionError::HostUnavailable.to_string()))?;
        let mut packages = Vec::new();
        for state in states.into_iter().filter(|state| state.enabled) {
            let installed = self.cache.resolve(&state.id, &state.active).map_err(|_| {
                CoreError::other(ProductExtensionError::PackageUnavailable.to_string())
            })?;
            if installed.manifest().code().is_some() {
                return Err(CoreError::other(
                    ProductExtensionError::CodeAuthorityRequired.to_string(),
                ));
            }
            packages.push(DeclarativePackage::load(&installed).map_err(|_| {
                CoreError::other(ProductExtensionError::PackageUnavailable.to_string())
            })?);
        }
        let plugin = ProductExtensionsPlugin::new(packages, true)
            .map_err(|error| CoreError::other(error.to_string()))?;
        for row in plugin.exact_inventory() {
            context.contribute(row.kind, row.name)?;
        }
        plugin.apply_generation(context)
    }
}

struct ProductExtensionsPlugin {
    packages: Vec<DeclarativePackage>,
    host: Arc<ProductHost>,
    dynamic_inventory: bool,
}

impl ProductExtensionsPlugin {
    fn new(
        packages: Vec<DeclarativePackage>,
        dynamic_inventory: bool,
    ) -> Result<Self, ProductExtensionError> {
        let host = Arc::new(ProductHost::new(&packages));
        // Run the host-neutral duplicate/package preflight now. Its plugin is
        // rebuilt during apply, but construction here ensures the public
        // constructor cannot return a generation known to be invalid.
        let _ = declarative_activation_plugin(
            packages.clone(),
            host.clone() as Arc<dyn DeclarativeContributionHost>,
        )
        .map_err(|_| ProductExtensionError::Duplicate)?;
        preflight_mcp_claims(&packages)?;
        Ok(Self {
            packages,
            host,
            dynamic_inventory,
        })
    }

    fn exact_inventory(&self) -> Vec<PluginContributionSpec> {
        let mut rows = self.declarative_inventory();
        rows.extend(self.packages.iter().flat_map(|package| {
            package.mcp_contributions().iter().map(|contribution| {
                PluginContributionSpec::new(
                    CoreContributionKind::McpServer,
                    product_id(contribution.public_name()),
                )
            })
        }));
        rows
    }

    fn declarative_inventory(&self) -> Vec<PluginContributionSpec> {
        self.packages
            .iter()
            .flat_map(DeclarativePackage::contributions)
            .flat_map(|contribution| self.host.inventory(contribution))
            .collect()
    }

    fn apply_declarative(&self, context: &mut Context) -> Result<(), CoreError> {
        let declarative = declarative_activation_plugin(
            self.packages.clone(),
            self.host.clone() as Arc<dyn DeclarativeContributionHost>,
        )
        .map_err(|error| CoreError::other(error.to_string()))?;
        declarative.apply(context)
    }

    fn apply_generation(&self, context: &mut Context) -> Result<(), CoreError> {
        self.apply_declarative(context)?;
        activate_bundled_mcp(context, &self.packages)
    }
}

impl Plugin for ProductExtensionsPlugin {
    fn name(&self) -> &'static str {
        PRODUCT_EXTENSIONS_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            PRODUCT_EXTENSIONS_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            PRODUCT_FAMILIES,
        )
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        if self.dynamic_inventory {
            Vec::new()
        } else {
            self.exact_inventory()
        }
    }

    fn inject(&self) -> &'static [ServiceKey] {
        PRODUCT_SERVICES
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        self.apply_generation(context)
    }
}

struct ProductHost {
    permissions: BTreeMap<String, BTreeSet<PluginPermission>>,
}

impl ProductHost {
    fn new(packages: &[DeclarativePackage]) -> Self {
        let permissions = packages
            .iter()
            .map(|package| {
                (
                    package.manifest().id().as_str().to_owned(),
                    package.manifest().permissions().iter().copied().collect(),
                )
            })
            .collect();
        Self { permissions }
    }

    fn requires(
        &self,
        contribution: &DeclarativeContribution,
        required: &[PluginPermission],
    ) -> Result<(), HostActivationFailure> {
        let permissions = self
            .permissions
            .get(contribution.package_id().as_str())
            .ok_or(HostActivationFailure::Unavailable)?;
        if required
            .iter()
            .all(|permission| permissions.contains(permission))
        {
            Ok(())
        } else {
            Err(HostActivationFailure::InvalidDefinition)
        }
    }
}

enum ProductRegistration {
    Skill(SkillRegistration),
    Command(CommandRegistration),
    Preset(SubagentPresetRegistration),
    Hook(HookRegistration),
    Theme(ThemeRegistration),
    Provider(ProviderRegistration),
}

impl ProductRegistration {
    fn withdraw(self) {
        match self {
            Self::Skill(registration) => drop(registration),
            Self::Command(registration) => drop(registration),
            Self::Preset(registration) => drop(registration),
            Self::Hook(registration) => drop(registration),
            Self::Theme(registration) => drop(registration),
            Self::Provider(registration) => drop(registration),
        }
    }
}

impl DeclarativeContributionRegistration for ProductRegistration {
    fn withdraw(self: Box<Self>) {
        (*self).withdraw();
    }
}

impl DeclarativeContributionHost for ProductHost {
    fn required_services(&self) -> &'static [ServiceKey] {
        PRODUCT_SERVICES
    }

    fn descriptor_families(&self) -> &'static [PluginContributionKind] {
        PRODUCT_FAMILIES
    }

    fn inventory(&self, contribution: &DeclarativeContribution) -> Vec<PluginContributionSpec> {
        let kind = match contribution.kind() {
            ContributionKind::Skill => CoreContributionKind::Skill,
            ContributionKind::Command => CoreContributionKind::Command,
            ContributionKind::Agent => CoreContributionKind::AgentPreset,
            ContributionKind::Hook => CoreContributionKind::Hook,
            ContributionKind::Theme => CoreContributionKind::Theme,
            ContributionKind::Provider => CoreContributionKind::InferenceProvider,
            ContributionKind::Mcp => CoreContributionKind::McpServer,
        };
        vec![PluginContributionSpec::new(
            kind,
            product_id(contribution.public_name()),
        )]
    }

    fn activate_skill(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        let mut skill = Skill::parse(contribution.document()).unwrap_or_else(|| Skill {
            name: String::new(),
            description: String::new(),
            disable_model_invocation: false,
            body: contribution.document().to_owned(),
        });
        let legacy_id = product_id(contribution.public_name());
        skill.name = contribution.public_name().to_owned();
        if skill.body.trim().is_empty() || skill.body.len() > MAX_TEXT_BYTES {
            return Err(HostActivationFailure::InvalidDefinition);
        }
        let skills = context
            .get::<SkillSet>(heycode_skills::SERVICE_SKILLS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = skills
            .register_owned_with_aliases(skill, vec![legacy_id])
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Skill(registration)))
    }

    fn activate_command(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        let document: CommandDocument = strict_json(contribution.document())?;
        validate_text(&document.description, 256)?;
        validate_text(&document.prompt, MAX_TEXT_BYTES)?;
        let id = product_id(contribution.public_name());
        let source = CommandSource::from_plugin(PRODUCT_EXTENSIONS_PLUGIN_ID)
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let descriptor = CommandDescriptor::new(
            id,
            document.description,
            vec![
                CommandArgument::optional("input", "Optional command input")
                    .map_err(|_| HostActivationFailure::InvalidDefinition)?
                    .variadic(),
            ],
            CommandTiming::ModelScheduling,
            source,
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let command = Arc::new(DeclarativePromptCommand {
            descriptor,
            prompt: document.prompt,
        });
        let commands = context
            .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = commands
            .register_owned(command)
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Command(registration)))
    }

    fn activate_agent(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        let document: AgentDocument = strict_json(contribution.document())?;
        validate_text(&document.display, 256)?;
        validate_text(&document.instructions, MAX_TEXT_BYTES)?;
        let provider = document
            .provider
            .map(SubagentProviderId::new)
            .transpose()
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let (seed, continuation) = document.mode.resolve();
        let preset = SubagentPreset::new(
            product_id(contribution.public_name()),
            document.display,
            document.instructions,
            provider,
            seed,
            continuation,
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?
        .with_description(document.description)
        .map_err(|_| HostActivationFailure::InvalidDefinition)?
        .with_config(document.config)
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let registry = context
            .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = registry
            .register_preset_owned(preset)
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Preset(registration)))
    }

    fn activate_hook(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.requires(contribution, &[PluginPermission::HookRegistration])?;
        let document: HookDocument = strict_json(contribution.document())?;
        let phase = document.phase.resolve();
        let event = document.event.resolve();
        let action = match document.action {
            HookActionDocument::Command { command } => {
                self.requires(contribution, &[PluginPermission::ProcessSpawn])?;
                validate_text(&command, MAX_TEXT_BYTES)?;
                HookAction::Command(command)
            }
            HookActionDocument::Prompt { prompt } => {
                validate_text(&prompt, MAX_TEXT_BYTES)?;
                HookAction::Prompt(prompt)
            }
            HookActionDocument::Subagent { agent, prompt } => {
                validate_text(&agent, 128)?;
                validate_text(&prompt, MAX_TEXT_BYTES)?;
                HookAction::Subagent { agent, prompt }
            }
            HookActionDocument::McpTool {
                server,
                tool,
                arguments,
            } => {
                self.requires(contribution, &[PluginPermission::McpConnect])?;
                validate_text(&server, 128)?;
                validate_text(&tool, 128)?;
                HookAction::McpTool {
                    server,
                    tool,
                    arguments,
                }
            }
        };
        let service = context
            .get::<HookService>(heycode_hooks::SERVICE_HOOKS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = service.register_owned(Hook {
            owner: product_id(contribution.public_name()),
            phase,
            event,
            action,
            project_scoped: document.project_scoped,
        });
        Ok(Box::new(ProductRegistration::Hook(registration)))
    }

    fn activate_theme(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        let document: ThemeDocument = strict_json(contribution.document())?;
        validate_text(&document.title, 256)?;
        let palette = [
            parse_rgb(&document.colors.accent)?,
            parse_rgb(&document.colors.success)?,
            parse_rgb(&document.colors.error)?,
            parse_rgb(&document.colors.warn)?,
            parse_rgb(&document.colors.text)?,
            parse_rgb(&document.colors.dim)?,
            parse_rgb(&document.colors.border)?,
            parse_rgb(
                document
                    .colors
                    .code
                    .as_deref()
                    .unwrap_or(&document.colors.accent),
            )?,
            parse_rgb(
                document
                    .colors
                    .prompt_background
                    .as_deref()
                    .unwrap_or(&document.colors.border),
            )?,
            parse_rgb(
                document
                    .colors
                    .prompt_glyph
                    .as_deref()
                    .unwrap_or(&document.colors.dim),
            )?,
            parse_rgb(
                document
                    .colors
                    .panel_title
                    .as_deref()
                    .unwrap_or(&document.colors.accent),
            )?,
        ];
        let theme = Theme::new(
            product_id(contribution.public_name()),
            document.title,
            palette,
        )
        .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let ui = context
            .get::<UiRegistry>(heycode_ui::SERVICE_UI)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = ui
            .register_theme_owned(theme)
            .map_err(map_duplicate_unavailable)?;
        Ok(Box::new(ProductRegistration::Theme(registration)))
    }

    fn activate_provider(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure> {
        self.requires(
            contribution,
            &[
                PluginPermission::NetworkAccess,
                PluginPermission::CredentialUse,
            ],
        )?;
        let document: ProviderDocument = strict_json(contribution.document())?;
        if document.protocol != ProviderProtocolDocument::OpenAiChatCompletions {
            return Err(HostActivationFailure::Unsupported);
        }
        validate_text(&document.display_name, 256)?;
        validate_text(&document.default_model, 256)?;
        let name = product_id(contribution.public_name());
        let descriptor = ProviderDescriptor {
            id: name.clone(),
            display_name: document.display_name,
            protocols: vec![ProviderProtocol::OpenAiChatCompletions],
        };
        let reference = CredentialReference::new(document.credential_reference)
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let reference_text = reference.as_str().to_owned();
        let kind = CredentialKind::new(document.credential_kind)
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let credentials = context
            .get::<CredentialsService>(heycode_credentials::SERVICE_CREDENTIALS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let http = context
            .get::<heycode_http::HttpService>(heycode_http::SERVICE_HTTP)
            .ok_or(HostActivationFailure::Unavailable)?;
        let config = OpenAiChatCompletionsConfig::with_credential(
            descriptor.clone(),
            document.base_url,
            RouteCredential::registry(
                (*credentials).clone(),
                CredentialQuery::new(reference, kind),
            ),
        );
        let adapter = OpenAiChatCompletionsAdapter::new(config, (*http).clone())
            .map_err(|_| HostActivationFailure::InvalidDefinition)?;
        let provider = Arc::new(DeclarativeChatProvider {
            name,
            default_model: document.default_model,
            descriptor,
            credential_reference: reference_text,
            adapter,
        });
        let providers = context
            .get::<ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .ok_or(HostActivationFailure::Unavailable)?;
        let registration = providers
            .register_owned(provider)
            .map_err(|_| HostActivationFailure::Duplicate)?;
        Ok(Box::new(ProductRegistration::Provider(registration)))
    }
}

struct DeclarativePromptCommand {
    descriptor: CommandDescriptor,
    prompt: String,
}

#[async_trait]
impl Command for DeclarativePromptCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let prompt = if args.trim().is_empty() {
            self.prompt.clone()
        } else {
            format!("{}\n\n{}", self.prompt, args.trim())
        };
        let _report = agent.send(&prompt).await?;
        Ok(())
    }
}

struct DeclarativeChatProvider {
    name: String,
    default_model: String,
    descriptor: ProviderDescriptor,
    credential_reference: String,
    adapter: OpenAiChatCompletionsAdapter,
}

impl Provider for DeclarativeChatProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.clone(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
    }

    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        ModelDescriptor::unknown(model)
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(&self.adapter)
    }

    fn stream(&self, _request: heycode_llm::ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "declarative provider dispatches through its inference adapter".to_owned(),
            ))
        }))
    }
}

fn activate_bundled_mcp(
    context: &mut Context,
    packages: &[DeclarativePackage],
) -> Result<(), CoreError> {
    let mut servers = HashMap::new();
    for package in packages {
        if package.mcp_contributions().is_empty() {
            continue;
        }
        let root = package.package_root().ok_or_else(|| {
            CoreError::other("plugin-bundled MCP requires a verified installed package root")
        })?;
        for contribution in package.mcp_contributions() {
            require_package_permissions(
                package,
                &[PluginPermission::McpConnect, PluginPermission::ProcessSpawn],
            )?;
            let id = product_id(contribution.public_name());
            let definition = bundled_mcp_definition(package, contribution, root, &id)?;
            if servers
                .insert(id, McpServerSpec::Definition(definition))
                .is_some()
            {
                return Err(CoreError::other(
                    ProductExtensionError::Duplicate.to_string(),
                ));
            }
        }
    }
    if servers.is_empty() {
        return Ok(());
    }
    let cwd = std::env::current_dir()
        .map_err(|_| CoreError::other(ProductExtensionError::HostUnavailable.to_string()))?;
    let plugin = heycode_mcp::mcp_plugin(servers, cwd);
    plugin.apply(context)
}

fn bundled_mcp_definition(
    package: &DeclarativePackage,
    contribution: &DeclarativeContribution,
    root: &Path,
    id: &str,
) -> Result<McpServerDefinition, CoreError> {
    let document: McpDocument = strict_json(contribution.document())
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    if document.transport != McpTransportDocument::Stdio {
        return Err(CoreError::other(
            ProductExtensionError::InvalidDefinition.to_string(),
        ));
    }
    let command = resolve_package_file(root, &document.command)?;
    let cwd = match document.cwd {
        Some(relative) => resolve_package_directory(root, &relative)?,
        None => root.to_path_buf(),
    };
    let arguments = document
        .args
        .into_iter()
        .map(McpArgument::literal)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    let mut environment = BTreeMap::new();
    for (name, value) in document.environment {
        let value = match value {
            McpEnvironmentDocument::Literal { value } => McpEnvironmentValue::literal(value),
            McpEnvironmentDocument::Credential { reference } => {
                require_package_permissions(package, &[PluginPermission::CredentialUse])?;
                heycode_mcp::McpSecretReference::new(reference).map(McpEnvironmentValue::credential)
            }
        }
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
        if environment.insert(name, value).is_some() {
            return Err(CoreError::other(
                ProductExtensionError::InvalidDefinition.to_string(),
            ));
        }
    }
    let transport = McpStdioTransport::new(
        command.to_string_lossy().into_owned(),
        cwd,
        arguments,
        environment,
    )
    .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    let timeouts = McpTimeouts::new(
        document.timeouts.startup_ms,
        document.timeouts.request_ms,
        document.timeouts.tool_ms,
        document.timeouts.shutdown_ms,
    )
    .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    let reconnect = McpReconnectPolicy::new(
        document.reconnect.enabled,
        document.reconnect.initial_delay_ms,
        document.reconnect.max_delay_ms,
        document.reconnect.max_attempts,
    )
    .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    let enabled_tools = document
        .tools
        .enabled
        .map(|rows| rows.into_iter().collect());
    let disabled_tools = document.tools.disabled.into_iter().collect();
    let per_tool_approval = document
        .tools
        .approval
        .into_iter()
        .map(|(name, mode)| (name, mode.resolve()))
        .collect();
    let tool_policy = McpToolPolicy::new(
        enabled_tools,
        disabled_tools,
        document.tools.default_approval.resolve(),
        per_tool_approval,
    )
    .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    McpServerDefinition::new(
        id,
        document
            .display_name
            .unwrap_or_else(|| contribution.public_name().to_owned()),
        McpDefinitionScope::User,
        McpTransportDefinition::Stdio(transport),
    )
    .map(|definition| {
        definition
            .with_enabled(document.enabled)
            .with_required(document.required)
            .with_timeouts(timeouts)
            .with_reconnect(reconnect)
            .with_tool_policy(tool_policy)
            .with_exposure(McpExposurePolicy {
                resources: document.exposure.resources,
                prompts: document.exposure.prompts,
                instructions: document.exposure.instructions,
            })
    })
    .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))
}

fn preflight_mcp_claims(packages: &[DeclarativePackage]) -> Result<(), ProductExtensionError> {
    let mut claims = BTreeSet::new();
    for contribution in packages
        .iter()
        .flat_map(DeclarativePackage::mcp_contributions)
    {
        if !claims.insert(product_id(contribution.public_name())) {
            return Err(ProductExtensionError::Duplicate);
        }
    }
    Ok(())
}

fn require_package_permissions(
    package: &DeclarativePackage,
    required: &[PluginPermission],
) -> Result<(), CoreError> {
    if required
        .iter()
        .all(|permission| package.manifest().permissions().contains(permission))
    {
        Ok(())
    } else {
        Err(CoreError::other(
            ProductExtensionError::MissingPermission.to_string(),
        ))
    }
}

fn resolve_package_file(root: &Path, relative: &str) -> Result<PathBuf, CoreError> {
    let path = resolve_package_path(root, relative)?;
    let metadata = std::fs::metadata(&path)
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    if !metadata.is_file() {
        return Err(CoreError::other(
            ProductExtensionError::InvalidDefinition.to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(CoreError::other(
                ProductExtensionError::InvalidDefinition.to_string(),
            ));
        }
    }
    Ok(path)
}

fn resolve_package_directory(root: &Path, relative: &str) -> Result<PathBuf, CoreError> {
    let path = resolve_package_path(root, relative)?;
    if !std::fs::metadata(&path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        return Err(CoreError::other(
            ProductExtensionError::InvalidDefinition.to_string(),
        ));
    }
    Ok(path)
}

fn resolve_package_path(root: &Path, relative: &str) -> Result<PathBuf, CoreError> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CoreError::other(
            ProductExtensionError::InvalidDefinition.to_string(),
        ));
    }
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    let canonical = std::fs::canonicalize(root.join(relative))
        .map_err(|_| CoreError::other(ProductExtensionError::InvalidDefinition.to_string()))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(CoreError::other(
            ProductExtensionError::InvalidDefinition.to_string(),
        ));
    }
    Ok(canonical)
}

fn product_id(public_name: &str) -> String {
    let mut local = public_name
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or("extension")
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while local.contains("--") {
        local = local.replace("--", "-");
    }
    let local = local.trim_matches('-');
    let local = if local.is_empty() { "extension" } else { local };
    let local = &local[..local.len().min(32)];
    let digest = Sha256::digest(public_name.as_bytes());
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("ext-{local}-{suffix}")
}

fn strict_json<T: for<'de> Deserialize<'de>>(document: &str) -> Result<T, HostActivationFailure> {
    if document.len() > MAX_TEXT_BYTES {
        return Err(HostActivationFailure::InvalidDefinition);
    }
    serde_json::from_str(document).map_err(|_| HostActivationFailure::InvalidDefinition)
}

fn validate_text(value: &str, maximum: usize) -> Result<(), HostActivationFailure> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(HostActivationFailure::InvalidDefinition);
    }
    Ok(())
}

fn parse_rgb(value: &str) -> Result<Rgb, HostActivationFailure> {
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes.first() != Some(&b'#') {
        return Err(HostActivationFailure::InvalidDefinition);
    }
    let component = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&value[range], 16).map_err(|_| HostActivationFailure::InvalidDefinition)
    };
    Ok(Rgb::new(
        component(1..3)?,
        component(3..5)?,
        component(5..7)?,
    ))
}

fn map_duplicate_unavailable<E: std::fmt::Display>(error: E) -> HostActivationFailure {
    if error.to_string().contains("already") || error.to_string().contains("duplicate") {
        HostActivationFailure::Duplicate
    } else {
        HostActivationFailure::Unavailable
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandDocument {
    description: String,
    prompt: String,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentDocument {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    config: heycode_agent::SubagentConfig,
    display: String,
    instructions: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    mode: AgentModeDocument,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentModeDocument {
    #[serde(rename = "oneshot")]
    #[default]
    OneShot,
    Continuable,
    Fork,
    #[serde(alias = "fork-continuable")]
    ForkContinuable,
}

impl AgentModeDocument {
    const fn resolve(self) -> (SubagentSeed, SubagentContinuation) {
        match self {
            Self::OneShot => (SubagentSeed::Fresh, SubagentContinuation::OneShot),
            Self::Continuable => (SubagentSeed::Fresh, SubagentContinuation::Continuable),
            Self::Fork => (SubagentSeed::ForkParent, SubagentContinuation::OneShot),
            Self::ForkContinuable => (SubagentSeed::ForkParent, SubagentContinuation::Continuable),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookDocument {
    phase: HookPhaseDocument,
    event: HookEventDocument,
    action: HookActionDocument,
    #[serde(default)]
    project_scoped: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HookPhaseDocument {
    Pre,
    Post,
}

impl HookPhaseDocument {
    const fn resolve(self) -> HookPhase {
        match self {
            Self::Pre => HookPhase::Pre,
            Self::Post => HookPhase::Post,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HookEventDocument {
    ToolUse,
    Turn,
    Session,
    UserPrompt,
    Subagent,
    McpServer,
}

impl HookEventDocument {
    const fn resolve(self) -> HookEvent {
        match self {
            Self::ToolUse => HookEvent::ToolUse,
            Self::Turn => HookEvent::Turn,
            Self::Session => HookEvent::Session,
            Self::UserPrompt => HookEvent::UserPrompt,
            Self::Subagent => HookEvent::Subagent,
            Self::McpServer => HookEvent::McpServer,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HookActionDocument {
    Command {
        command: String,
    },
    Prompt {
        prompt: String,
    },
    Subagent {
        agent: String,
        prompt: String,
    },
    McpTool {
        server: String,
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeDocument {
    title: String,
    colors: ThemeColorsDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeColorsDocument {
    accent: String,
    success: String,
    error: String,
    warn: String,
    text: String,
    dim: String,
    border: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    prompt_background: Option<String>,
    #[serde(default)]
    prompt_glyph: Option<String>,
    #[serde(default)]
    panel_title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderDocument {
    protocol: ProviderProtocolDocument,
    display_name: String,
    base_url: String,
    default_model: String,
    credential_reference: String,
    credential_kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProviderProtocolDocument {
    OpenAiChatCompletions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDocument {
    transport: McpTransportDocument,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, McpEnvironmentDocument>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    required: bool,
    #[serde(default)]
    timeouts: McpTimeoutDocument,
    #[serde(default)]
    reconnect: McpReconnectDocument,
    #[serde(default)]
    tools: McpToolsDocument,
    #[serde(default)]
    exposure: McpExposureDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum McpTransportDocument {
    Stdio,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum McpEnvironmentDocument {
    Literal { value: String },
    Credential { reference: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTimeoutDocument {
    #[serde(default = "default_mcp_startup")]
    startup_ms: u64,
    #[serde(default = "default_mcp_request")]
    request_ms: u64,
    #[serde(default = "default_mcp_tool")]
    tool_ms: u64,
    #[serde(default = "default_mcp_shutdown")]
    shutdown_ms: u64,
}

impl Default for McpTimeoutDocument {
    fn default() -> Self {
        Self {
            startup_ms: default_mcp_startup(),
            request_ms: default_mcp_request(),
            tool_ms: default_mcp_tool(),
            shutdown_ms: default_mcp_shutdown(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpReconnectDocument {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_reconnect_initial")]
    initial_delay_ms: u64,
    #[serde(default = "default_reconnect_max")]
    max_delay_ms: u64,
    #[serde(default = "default_reconnect_attempts")]
    max_attempts: u32,
}

impl Default for McpReconnectDocument {
    fn default() -> Self {
        Self {
            enabled: false,
            initial_delay_ms: default_reconnect_initial(),
            max_delay_ms: default_reconnect_max(),
            max_attempts: default_reconnect_attempts(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct McpToolsDocument {
    #[serde(default)]
    enabled: Option<Vec<String>>,
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    default_approval: McpApprovalDocument,
    #[serde(default)]
    approval: BTreeMap<String, McpApprovalDocument>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum McpApprovalDocument {
    #[default]
    Prompt,
    Allow,
    Deny,
}

impl McpApprovalDocument {
    const fn resolve(self) -> McpApprovalMode {
        match self {
            Self::Prompt => McpApprovalMode::Prompt,
            Self::Allow => McpApprovalMode::Allow,
            Self::Deny => McpApprovalMode::Deny,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct McpExposureDocument {
    #[serde(default)]
    resources: bool,
    #[serde(default)]
    prompts: bool,
    #[serde(default)]
    instructions: bool,
}

const fn default_true() -> bool {
    true
}
const fn default_mcp_startup() -> u64 {
    30_000
}
const fn default_mcp_request() -> u64 {
    30_000
}
const fn default_mcp_tool() -> u64 {
    60_000
}
const fn default_mcp_shutdown() -> u64 {
    5_000
}
const fn default_reconnect_initial() -> u64 {
    500
}
const fn default_reconnect_max() -> u64 {
    30_000
}
const fn default_reconnect_attempts() -> u32 {
    10
}

mod agent_import;
pub use agent_import::{AgentImportFormat, import_agent};

mod agent_management;
pub use agent_management::{AgentDeclarationService, SERVICE_AGENT_DECLARATIONS};
