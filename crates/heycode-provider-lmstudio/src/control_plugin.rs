//! PLM04 product integration for explicit LM Studio model control.
//!
//! The provider's raw load/unload client stays in `model_control`; this module
//! joins it to live Settings, the command registry and catalog refresh. A load
//! happens only after an explicit `/lmstudio load <model>` command. Every
//! operation reads the current Settings snapshot, performs provider readback,
//! refreshes the shared model generation and only then publishes success.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandSource, CommandTiming,
};
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_llm::{CatalogRefreshMode, CatalogRegistry};
use heycode_settings::{SettingsDefinition, SettingsNamespace, SettingsSchema, SettingsService};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    LM_STUDIO_PROVIDER, LmStudioCatalog, LmStudioControlError, LmStudioLoadSettings,
    LmStudioModelControl, LmStudioUnloadPlan, SERVICE_LM_STUDIO_MODEL_CONTROL,
    SERVICE_LM_STUDIO_MODELS, validate_agent_route,
};

/// Settings namespace for explicit LM Studio load-time controls.
pub const LM_STUDIO_CONTROL_SETTINGS_NAMESPACE: &str = "lmstudio-load";

const DEFAULT_CONTEXT_LENGTH: u64 = 4_096;
const DEFAULT_EVAL_BATCH_SIZE: u64 = 512;
const DEFAULT_NUM_EXPERTS: u64 = 1;
const MAX_CONTEXT_LENGTH: u64 = 4_194_304;
const MAX_EVAL_BATCH_SIZE: u64 = 1_048_576;
const MAX_NUM_EXPERTS: u64 = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum NumericMode {
    ServerDefault,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BooleanMode {
    ServerDefault,
    Enabled,
    Disabled,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferencesWire {
    context_length_mode: NumericMode,
    context_length: u64,
    eval_batch_size_mode: NumericMode,
    eval_batch_size: u64,
    flash_attention: BooleanMode,
    num_experts_mode: NumericMode,
    num_experts: u64,
    offload_kv_cache_to_gpu: BooleanMode,
}

/// Resolved, validated load controls read at operation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioControlPreferences {
    settings: LmStudioLoadSettings,
}

impl LmStudioControlPreferences {
    /// Convert one authoritative Settings value into provider load controls.
    ///
    /// # Errors
    /// Unknown fields, invalid modes, zero explicit values or values beyond
    /// the bounded product limits fail before any provider request.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, LmStudioProductControlError> {
        let wire: PreferencesWire = serde_json::from_value(value.clone())
            .map_err(|_| LmStudioProductControlError::InvalidSettings)?;
        validate_bounded(wire.context_length, MAX_CONTEXT_LENGTH)?;
        validate_bounded(wire.eval_batch_size, MAX_EVAL_BATCH_SIZE)?;
        validate_bounded(wire.num_experts, MAX_NUM_EXPERTS)?;

        let mut settings = LmStudioLoadSettings::new();
        if wire.context_length_mode == NumericMode::Explicit {
            settings = settings
                .with_context_length(wire.context_length)
                .map_err(map_provider_control)?;
        }
        if wire.eval_batch_size_mode == NumericMode::Explicit {
            settings = settings
                .with_eval_batch_size(wire.eval_batch_size)
                .map_err(map_provider_control)?;
        }
        settings = match wire.flash_attention {
            BooleanMode::ServerDefault => settings,
            BooleanMode::Enabled => settings.with_flash_attention(true),
            BooleanMode::Disabled => settings.with_flash_attention(false),
        };
        if wire.num_experts_mode == NumericMode::Explicit {
            settings = settings
                .with_num_experts(wire.num_experts)
                .map_err(map_provider_control)?;
        }
        settings = match wire.offload_kv_cache_to_gpu {
            BooleanMode::ServerDefault => settings,
            BooleanMode::Enabled => settings.with_offload_kv_cache_to_gpu(true),
            BooleanMode::Disabled => settings.with_offload_kv_cache_to_gpu(false),
        };
        Ok(Self { settings })
    }

    /// Provider controls produced by the resolved preferences.
    #[must_use]
    pub const fn load_settings(&self) -> &LmStudioLoadSettings {
        &self.settings
    }
}

/// Build the live Settings contract rendered by U14.
///
/// Numeric modes make omission explicit without using zero as a hidden
/// sentinel. Values remain visible/editable while their matching mode says
/// whether they are sent.
///
/// # Errors
/// Static schema/default/namespace construction failure.
pub fn lmstudio_control_settings_definition()
-> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let defaults = serde_json::json!({
        "context_length_mode": "server-default",
        "context_length": DEFAULT_CONTEXT_LENGTH,
        "eval_batch_size_mode": "server-default",
        "eval_batch_size": DEFAULT_EVAL_BATCH_SIZE,
        "flash_attention": "server-default",
        "num_experts_mode": "server-default",
        "num_experts": DEFAULT_NUM_EXPERTS,
        "offload_kv_cache_to_gpu": "server-default"
    });
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "context_length_mode": {
                    "type": "string",
                    "enum": ["server-default", "explicit"]
                },
                "context_length": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_CONTEXT_LENGTH
                },
                "eval_batch_size_mode": {
                    "type": "string",
                    "enum": ["server-default", "explicit"]
                },
                "eval_batch_size": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_EVAL_BATCH_SIZE
                },
                "flash_attention": {
                    "type": "string",
                    "enum": ["server-default", "enabled", "disabled"]
                },
                "num_experts_mode": {
                    "type": "string",
                    "enum": ["server-default", "explicit"]
                },
                "num_experts": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_NUM_EXPERTS
                },
                "offload_kv_cache_to_gpu": {
                    "type": "string",
                    "enum": ["server-default", "enabled", "disabled"]
                }
            }
        }),
        defaults,
        |value| {
            LmStudioControlPreferences::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?;
    Ok(SettingsDefinition::new(
        SettingsNamespace::new(LM_STUDIO_CONTROL_SETTINGS_NAMESPACE)?,
        schema,
    ))
}

/// Successful explicit product operation after provider readback and shared
/// catalog refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LmStudioProductControlReceipt {
    /// One exact model instance was loaded.
    Loaded {
        /// Provider-returned validated instance id.
        instance_id: String,
        /// Shared catalog revision committed after readback.
        catalog_revision: u64,
    },
    /// One exact instance was unloaded.
    Unloaded {
        /// Provider-returned validated instance id.
        instance_id: String,
        /// Shared catalog revision committed after readback.
        catalog_revision: u64,
    },
}

/// Product owner for Settings-backed explicit load/unload operations.
#[derive(Clone)]
pub struct LmStudioControlOperations {
    settings: Arc<SettingsService>,
    control: Arc<LmStudioModelControl>,
    catalog: Arc<LmStudioCatalog>,
    catalogs: Arc<CatalogRegistry>,
    lifecycle: CancellationToken,
}

impl LmStudioControlOperations {
    /// Bind the exact services contributed by LM Studio and the shared world.
    #[must_use]
    pub const fn new(
        settings: Arc<SettingsService>,
        control: Arc<LmStudioModelControl>,
        catalog: Arc<LmStudioCatalog>,
        catalogs: Arc<CatalogRegistry>,
        lifecycle: CancellationToken,
    ) -> Self {
        Self {
            settings,
            control,
            catalog,
            catalogs,
            lifecycle,
        }
    }

    /// Explicitly load one downloaded, tool-capable chat model with the latest
    /// Settings snapshot, then verify and refresh the catalog.
    ///
    /// # Errors
    /// Settings, cancellation, catalog, eligibility, already-loaded,
    /// provider, readback or refresh failures publish no success receipt.
    pub async fn load(
        &self,
        model: &str,
    ) -> Result<LmStudioProductControlReceipt, LmStudioProductControlError> {
        validate_target(model)?;
        let cancellation = self.lifecycle.child_token();
        if cancellation.is_cancelled() {
            return Err(LmStudioProductControlError::Cancelled);
        }
        let preferences = self.preferences()?;
        let before = self
            .catalog
            .list_models(cancellation.child_token())
            .await
            .map_err(|_| LmStudioProductControlError::CatalogUnavailable)?;
        let record = validate_agent_route(&before, model)
            .map_err(|_| LmStudioProductControlError::RouteRefused)?;
        if record.is_loaded() {
            return Err(LmStudioProductControlError::AlreadyLoaded);
        }
        let plan = self
            .control
            .prepare_load(record, preferences.settings)
            .map_err(map_provider_control)?;
        let receipt = self
            .control
            .load(plan, cancellation.child_token())
            .await
            .map_err(map_provider_control)?;
        let instance_id = receipt.instance_id().to_owned();
        let after = self
            .catalog
            .list_models(cancellation.child_token())
            .await
            .map_err(|_| LmStudioProductControlError::ReadbackUnavailable)?;
        let confirmed = after.iter().any(|record| {
            record.key == model
                && record
                    .loaded_instances
                    .iter()
                    .any(|instance| instance.id == instance_id)
        });
        if !confirmed {
            return Err(LmStudioProductControlError::ReadbackMismatch);
        }
        let revision = self.refresh(cancellation).await?;
        Ok(LmStudioProductControlReceipt::Loaded {
            instance_id,
            catalog_revision: revision,
        })
    }

    /// Explicitly unload one exact observed instance, then verify and refresh
    /// the catalog.
    ///
    /// # Errors
    /// Invalid/unknown instance, cancellation, provider, readback or refresh
    /// failures publish no success receipt.
    pub async fn unload(
        &self,
        instance_id: &str,
    ) -> Result<LmStudioProductControlReceipt, LmStudioProductControlError> {
        validate_target(instance_id)?;
        let cancellation = self.lifecycle.child_token();
        if cancellation.is_cancelled() {
            return Err(LmStudioProductControlError::Cancelled);
        }
        let before = self
            .catalog
            .list_models(cancellation.child_token())
            .await
            .map_err(|_| LmStudioProductControlError::CatalogUnavailable)?;
        if !before.iter().any(|record| {
            record
                .loaded_instances
                .iter()
                .any(|instance| instance.id == instance_id)
        }) {
            return Err(LmStudioProductControlError::UnknownInstance);
        }
        let plan = LmStudioUnloadPlan::new(instance_id).map_err(map_provider_control)?;
        let receipt = self
            .control
            .unload(plan, cancellation.child_token())
            .await
            .map_err(map_provider_control)?;
        let instance_id = receipt.instance_id().to_owned();
        let after = self
            .catalog
            .list_models(cancellation.child_token())
            .await
            .map_err(|_| LmStudioProductControlError::ReadbackUnavailable)?;
        if after.iter().any(|record| {
            record
                .loaded_instances
                .iter()
                .any(|instance| instance.id == instance_id)
        }) {
            return Err(LmStudioProductControlError::ReadbackMismatch);
        }
        let revision = self.refresh(cancellation).await?;
        Ok(LmStudioProductControlReceipt::Unloaded {
            instance_id,
            catalog_revision: revision,
        })
    }

    fn preferences(&self) -> Result<LmStudioControlPreferences, LmStudioProductControlError> {
        let namespace = SettingsNamespace::new(LM_STUDIO_CONTROL_SETTINGS_NAMESPACE)
            .map_err(|_| LmStudioProductControlError::InvalidSettings)?;
        let snapshot = self
            .settings
            .get(&namespace)
            .map_err(|_| LmStudioProductControlError::SettingsUnavailable)?
            .ok_or(LmStudioProductControlError::SettingsUnavailable)?;
        LmStudioControlPreferences::from_value(snapshot.resolved())
    }

    async fn refresh(
        &self,
        cancellation: CancellationToken,
    ) -> Result<u64, LmStudioProductControlError> {
        let view = self
            .catalogs
            .refresh(LM_STUDIO_PROVIDER, CatalogRefreshMode::Force, cancellation)
            .await
            .map_err(|_| LmStudioProductControlError::CatalogRefreshFailed)?;
        Ok(view.snapshot.revision)
    }
}

impl std::fmt::Debug for LmStudioControlOperations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LmStudioControlOperations")
            .field("cancelled", &self.lifecycle.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Stable PLM04 product-control failure. No variant carries URLs, response
/// bodies or settings values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LmStudioProductControlError {
    /// Settings document has the wrong shape or an out-of-range explicit value.
    #[error("LM Studio load settings are invalid")]
    InvalidSettings,
    /// Settings namespace could not be read.
    #[error("LM Studio load settings are unavailable")]
    SettingsUnavailable,
    /// Model/instance argument failed the bounded identifier grammar.
    #[error("LM Studio model-control target is invalid")]
    InvalidTarget,
    /// Local library could not be read before mutation.
    #[error("LM Studio model catalog is unavailable")]
    CatalogUnavailable,
    /// Model is unknown, non-chat or lacks affirmative tool-use evidence.
    #[error("LM Studio model is not eligible for agent-mode loading")]
    RouteRefused,
    /// Explicit context length exceeds the selected model's published maximum.
    #[error("LM Studio load context exceeds the selected model maximum")]
    ContextExceedsModel,
    /// Explicit load was requested for an instance already in memory.
    #[error("LM Studio model is already loaded; unload its exact instance before reloading")]
    AlreadyLoaded,
    /// Explicit unload did not name an observed live instance.
    #[error("LM Studio has no loaded instance with that id")]
    UnknownInstance,
    /// Provider-local load/unload request failed.
    #[error("LM Studio model-control operation failed")]
    ProviderOperation,
    /// Provider mutation settled but authoritative local readback failed.
    #[error("LM Studio accepted the operation but model-state readback is unavailable")]
    ReadbackUnavailable,
    /// Provider mutation settled but readback did not confirm the exact effect.
    #[error("LM Studio model-state readback did not confirm the exact operation")]
    ReadbackMismatch,
    /// Shared picker generation could not refresh after confirmed readback.
    #[error("LM Studio operation succeeded but the shared model catalog could not refresh")]
    CatalogRefreshFailed,
    /// Product lifecycle cancelled the operation.
    #[error("LM Studio model-control operation was cancelled")]
    Cancelled,
}

struct LmStudioCommand {
    descriptor: CommandDescriptor,
    operations: LmStudioControlOperations,
    shutdown: CommandAvailability,
}

#[async_trait]
impl Command for LmStudioCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.operations.lifecycle.is_cancelled() {
            self.shutdown.clone()
        } else {
            CommandAvailability::available()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let (operation, target) = parse_args(args)?;
        let receipt = match operation {
            "load" => self.operations.load(target).await?,
            "unload" => self.operations.unload(target).await?,
            _ => anyhow::bail!("usage: /lmstudio <load|unload> <model-or-instance>"),
        };
        let text = match receipt {
            LmStudioProductControlReceipt::Loaded {
                instance_id,
                catalog_revision,
            } => format!(
                "LM Studio loaded instance `{instance_id}`; model catalog revision {catalog_revision}"
            ),
            LmStudioProductControlReceipt::Unloaded {
                instance_id,
                catalog_revision,
            } => format!(
                "LM Studio unloaded instance `{instance_id}`; model catalog revision {catalog_revision}"
            ),
        };
        agent.ui().emit(heycode_agent::UiEvent::Info { text });
        Ok(())
    }
}

/// Register the live load settings namespace and queued `/lmstudio` command.
///
/// The raw control/catalog services are provider-owned; this plugin is their
/// product Consumer. Its lifecycle token is registered last so shutdown first
/// cancels any active HTTP wait, then removes command/settings effects in LIFO
/// order.
#[must_use]
pub fn lmstudio_control_plugin() -> Box<dyn Plugin> {
    struct LmStudioControlPlugin;

    impl Plugin for LmStudioControlPlugin {
        fn name(&self) -> &'static str {
            "lmstudio-control"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    LM_STUDIO_CONTROL_SETTINGS_NAMESPACE,
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "lmstudio",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_agent::SERVICE_COMMANDS,
                SERVICE_LM_STUDIO_MODEL_CONTROL,
                SERVICE_LM_STUDIO_MODELS,
                heycode_llm::SERVICE_MODELS,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = get::<SettingsService>(context, heycode_settings::SERVICE_SETTINGS)?;
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let control = get::<LmStudioModelControl>(context, SERVICE_LM_STUDIO_MODEL_CONTROL)?;
            let catalog = get::<LmStudioCatalog>(context, SERVICE_LM_STUDIO_MODELS)?;
            let catalogs = get::<CatalogRegistry>(context, heycode_llm::SERVICE_MODELS)?;
            settings
                .register(
                    context,
                    lmstudio_control_settings_definition()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let lifecycle = CancellationToken::new();
            let operations = LmStudioControlOperations::new(
                settings,
                control,
                catalog,
                catalogs,
                lifecycle.clone(),
            );
            let descriptor = CommandDescriptor::new(
                "lmstudio",
                "Explicitly load or unload an LM Studio model instance",
                vec![
                    CommandArgument::required("operation", "load or unload")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    CommandArgument::required("target", "model id or loaded instance id")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                ],
                CommandTiming::Queued,
                CommandSource::from_plugin(self.name())
                    .map_err(|error| CoreError::other(error.to_string()))?,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let shutdown =
                CommandAvailability::unavailable("LM Studio model control is shutting down")
                    .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(LmStudioCommand {
                        descriptor,
                        operations,
                        shutdown,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.effect(move || lifecycle.cancel());
            Ok(())
        }
    }

    Box::new(LmStudioControlPlugin)
}

fn validate_bounded(value: u64, maximum: u64) -> Result<(), LmStudioProductControlError> {
    if value == 0 || value > maximum {
        Err(LmStudioProductControlError::InvalidSettings)
    } else {
        Ok(())
    }
}

fn validate_target(value: &str) -> Result<(), LmStudioProductControlError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        Err(LmStudioProductControlError::InvalidTarget)
    } else {
        Ok(())
    }
}

fn map_provider_control(error: LmStudioControlError) -> LmStudioProductControlError {
    match error {
        LmStudioControlError::InvalidSettings => LmStudioProductControlError::InvalidSettings,
        LmStudioControlError::ContextExceedsModel => {
            LmStudioProductControlError::ContextExceedsModel
        }
        LmStudioControlError::ExplicitLoadRequired => LmStudioProductControlError::RouteRefused,
        LmStudioControlError::InvalidInstanceId => LmStudioProductControlError::InvalidTarget,
        LmStudioControlError::Cancelled => LmStudioProductControlError::Cancelled,
        LmStudioControlError::CredentialUnavailable
        | LmStudioControlError::Unauthorized
        | LmStudioControlError::Unavailable
        | LmStudioControlError::Network
        | LmStudioControlError::InvalidResponse
        | LmStudioControlError::SettingsMismatch => LmStudioProductControlError::ProviderOperation,
    }
}

fn parse_args(args: &str) -> anyhow::Result<(&str, &str)> {
    let mut parts = args.split_whitespace();
    let operation = parts.next();
    let target = parts.next();
    if parts.next().is_some() {
        anyhow::bail!("usage: /lmstudio <load|unload> <model-or-instance>");
    }
    match (operation, target) {
        (Some(operation @ ("load" | "unload")), Some(target)) => Ok((operation, target)),
        _ => anyhow::bail!("usage: /lmstudio <load|unload> <model-or-instance>"),
    }
}

fn get<T: Send + Sync + 'static>(
    context: &Context,
    key: heycode_core::ServiceKey,
) -> CoreResult<Arc<T>> {
    context
        .get::<T>(key)
        .ok_or_else(|| CoreError::MissingService(key.to_string()))
}
