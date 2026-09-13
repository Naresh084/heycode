//! Restart-applied PAWS06 cache, guardrail, and route-metadata policy.

use heycode_authorization_aws::AwsRegion;
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::CredentialQuery;
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsFieldPath, SettingsNamespace, SettingsSchema,
    SettingsService, SettingsSnapshot,
};
use serde::Deserialize;

use crate::{
    AwsInferencePluginConfig, AwsInferencePluginError, BedrockCachePlacement, BedrockCachePoint,
    BedrockCacheTtl, BedrockConverseModelEvidence, BedrockGuardrailConfig,
    BedrockGuardrailStreamMode, BedrockGuardrailTrace, BedrockMetadataError,
    BedrockMetadataErrorClass, BedrockPromptCacheCapabilities, BedrockPromptCacheConfig,
    BedrockRuntimeRequestMetadata, aws_inference_plugin,
};

/// Settings namespace owned by the PAWS06 policy plugin.
pub const AWS_BEDROCK_SETTINGS_NAMESPACE: &str = "aws-bedrock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CacheMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CachePlacementWire {
    Tools,
    System,
    LatestUserMessage,
}

impl CachePlacementWire {
    const fn into_policy(self) -> BedrockCachePlacement {
        match self {
            Self::Tools => BedrockCachePlacement::Tools,
            Self::System => BedrockCachePlacement::System,
            Self::LatestUserMessage => BedrockCachePlacement::LatestUserMessage,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CacheTtlWire {
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

impl CacheTtlWire {
    const fn into_policy(self) -> BedrockCacheTtl {
        match self {
            Self::FiveMinutes => BedrockCacheTtl::DefaultFiveMinutes,
            Self::OneHour => BedrockCacheTtl::OneHour,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachePointWire {
    placement: CachePlacementWire,
    ttl: CacheTtlWire,
}

impl CachePointWire {
    const fn into_policy(self) -> BedrockCachePoint {
        BedrockCachePoint::new(self.placement.into_policy(), self.ttl.into_policy())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptCacheWire {
    mode: CacheMode,
    points: Vec<CachePointWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum GuardrailMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum GuardrailTraceWire {
    ProviderDefault,
    Enabled,
    Disabled,
    EnabledFull,
}

impl GuardrailTraceWire {
    const fn into_policy(self) -> Option<BedrockGuardrailTrace> {
        match self {
            Self::ProviderDefault => None,
            Self::Enabled => Some(BedrockGuardrailTrace::Enabled),
            Self::Disabled => Some(BedrockGuardrailTrace::Disabled),
            Self::EnabledFull => Some(BedrockGuardrailTrace::EnabledFull),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum GuardrailStreamModeWire {
    ProviderDefault,
    Sync,
    Async,
}

impl GuardrailStreamModeWire {
    const fn into_policy(self) -> Option<BedrockGuardrailStreamMode> {
        match self {
            Self::ProviderDefault => None,
            Self::Sync => Some(BedrockGuardrailStreamMode::Sync),
            Self::Async => Some(BedrockGuardrailStreamMode::Async),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardrailWire {
    mode: GuardrailMode,
    identifier: String,
    version: String,
    trace: GuardrailTraceWire,
    stream_mode: GuardrailStreamModeWire,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsWire {
    prompt_cache: PromptCacheWire,
    guardrail: GuardrailWire,
}

/// One resolved PAWS06 Settings generation.
#[derive(Clone, PartialEq, Eq)]
pub struct AwsBedrockSettings {
    cache_points: Option<Vec<CachePointWire>>,
    guardrail: Option<BedrockGuardrailConfig>,
}

impl std::fmt::Debug for AwsBedrockSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AwsBedrockSettings")
            .field(
                "cache_points",
                &self.cache_points.as_ref().map(std::vec::Vec::len),
            )
            .field("guardrail", &self.guardrail)
            .finish()
    }
}

impl AwsBedrockSettings {
    /// Parse one complete resolved namespace value.
    ///
    /// This validates request intent only. Cache capability evidence is a
    /// separate input and is never minted from Settings.
    ///
    /// # Errors
    /// Missing/unknown fields, contradictory enablement, or invalid guardrail
    /// metadata fail without echoing rejected values.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, AwsBedrockSettingsError> {
        let wire: SettingsWire = serde_json::from_value(value.clone())
            .map_err(|_| AwsBedrockSettingsError::InvalidSettings)?;
        validate_cache_points(&wire.prompt_cache.points)?;
        let cache_points = match wire.prompt_cache.mode {
            CacheMode::Disabled => None,
            CacheMode::Enabled if wire.prompt_cache.points.is_empty() => {
                return Err(AwsBedrockSettingsError::InvalidSettings);
            }
            CacheMode::Enabled => Some(wire.prompt_cache.points),
        };
        let guardrail = parse_guardrail(wire.guardrail)?;
        Ok(Self {
            cache_points,
            guardrail,
        })
    }

    /// Parse the exact registered restart-applied PAWS06 snapshot.
    ///
    /// # Errors
    /// Wrong namespace/application timing, unproved wire exposure, or an
    /// invalid resolved value fails closed.
    pub fn from_snapshot(snapshot: &SettingsSnapshot) -> Result<Self, AwsBedrockSettingsError> {
        if snapshot.namespace().as_str() != AWS_BEDROCK_SETTINGS_NAMESPACE
            || snapshot.applies() != SettingsApplies::Restart
            || !snapshot.wire_exposed()
        {
            return Err(AwsBedrockSettingsError::Unavailable);
        }
        Self::from_value(snapshot.resolved())
    }

    /// Convert this policy into exact runtime metadata for one selected model.
    ///
    /// Route metadata is always retained. Cache intent additionally requires
    /// exact, separately supplied selected-model evidence; missing and Unknown
    /// evidence never become support.
    ///
    /// # Errors
    /// Invalid/unsupported/unproven cache evidence fails before a provider
    /// option or inference config exists.
    pub fn runtime_metadata(
        &self,
        selected_model: &str,
        cache_capabilities: Option<BedrockPromptCacheCapabilities>,
    ) -> Result<BedrockRuntimeRequestMetadata, AwsBedrockSettingsError> {
        let prompt_cache = match &self.cache_points {
            None => None,
            Some(points) => {
                let capabilities =
                    cache_capabilities.ok_or(AwsBedrockSettingsError::CacheEvidenceUnproven)?;
                if capabilities.model_id() != selected_model {
                    return Err(AwsBedrockSettingsError::CacheEvidenceUnproven);
                }
                let points = points
                    .iter()
                    .copied()
                    .map(CachePointWire::into_policy)
                    .collect();
                Some(
                    BedrockPromptCacheConfig::new(points, capabilities)
                        .map_err(map_metadata_error)?,
                )
            }
        };
        Ok(BedrockRuntimeRequestMetadata::new(
            prompt_cache,
            self.guardrail.clone(),
        ))
    }
}

fn validate_cache_points(points: &[CachePointWire]) -> Result<(), AwsBedrockSettingsError> {
    if points.len() > 3 {
        return Err(AwsBedrockSettingsError::InvalidSettings);
    }
    let mut ordered = points.to_vec();
    ordered.sort_by_key(|point| point.placement.into_policy());
    let mut placements = std::collections::BTreeSet::new();
    let mut saw_five_minutes = false;
    for point in ordered {
        if !placements.insert(point.placement.into_policy()) {
            return Err(AwsBedrockSettingsError::InvalidSettings);
        }
        match point.ttl {
            CacheTtlWire::FiveMinutes => saw_five_minutes = true,
            CacheTtlWire::OneHour if saw_five_minutes => {
                return Err(AwsBedrockSettingsError::InvalidSettings);
            }
            CacheTtlWire::OneHour => {}
        }
    }
    Ok(())
}

fn parse_guardrail(
    wire: GuardrailWire,
) -> Result<Option<BedrockGuardrailConfig>, AwsBedrockSettingsError> {
    let has_identifier = !wire.identifier.is_empty();
    let has_version = !wire.version.is_empty();
    let configured = match (has_identifier, has_version) {
        (false, false) => None,
        (true, true) => {
            let mut guardrail = BedrockGuardrailConfig::new(wire.identifier, wire.version)
                .map_err(|_| AwsBedrockSettingsError::InvalidSettings)?;
            if let Some(trace) = wire.trace.into_policy() {
                guardrail = guardrail.with_trace(trace);
            }
            if let Some(mode) = wire.stream_mode.into_policy() {
                guardrail = guardrail.with_stream_mode(mode);
            }
            Some(guardrail)
        }
        (false, true) | (true, false) => {
            return Err(AwsBedrockSettingsError::InvalidSettings);
        }
    };
    match wire.mode {
        GuardrailMode::Disabled => Ok(None),
        GuardrailMode::Enabled => configured
            .map(Some)
            .ok_or(AwsBedrockSettingsError::InvalidSettings),
    }
}

/// Closed PAWS06 Settings/configuration failure without rejected values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AwsBedrockSettingsError {
    /// The namespace or Settings service is unavailable.
    Unavailable,
    /// The resolved Settings shape or provider metadata is invalid.
    InvalidSettings,
    /// Selected-model evidence explicitly denies the cache request.
    CacheEvidenceUnsupported,
    /// Selected-model evidence is missing, mismatched, or Unknown.
    CacheEvidenceUnproven,
    /// The resulting AWS inference configuration is inconsistent.
    InvalidInferenceConfig,
}

impl std::fmt::Display for AwsBedrockSettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Amazon Bedrock settings are unavailable",
            Self::InvalidSettings => "Amazon Bedrock settings are invalid",
            Self::CacheEvidenceUnsupported => {
                "Amazon Bedrock cache policy is unsupported by selected-model evidence"
            }
            Self::CacheEvidenceUnproven => {
                "Amazon Bedrock cache policy is unproven for the selected model"
            }
            Self::InvalidInferenceConfig => "Amazon Bedrock inference configuration is invalid",
        })
    }
}

impl std::error::Error for AwsBedrockSettingsError {}

fn map_metadata_error(error: BedrockMetadataError) -> AwsBedrockSettingsError {
    match error.class() {
        BedrockMetadataErrorClass::Invalid => AwsBedrockSettingsError::InvalidSettings,
        BedrockMetadataErrorClass::Unsupported => AwsBedrockSettingsError::CacheEvidenceUnsupported,
        BedrockMetadataErrorClass::Unproven => AwsBedrockSettingsError::CacheEvidenceUnproven,
    }
}

fn map_inference_error(_error: AwsInferencePluginError) -> AwsBedrockSettingsError {
    AwsBedrockSettingsError::InvalidInferenceConfig
}

/// Exact Settings namespace identity.
///
/// # Errors
/// Static namespace validation failure.
pub fn aws_bedrock_settings_namespace() -> Result<SettingsNamespace, heycode_settings::SettingsError>
{
    SettingsNamespace::new(AWS_BEDROCK_SETTINGS_NAMESPACE)
}

/// Build the restart-applied, wire-visible PAWS06 Settings contract.
///
/// Cache and guardrail behavior default to disabled. Route metadata has no
/// toggle because it is exact request provenance derived from the configured
/// region and selected target, not provider behavior.
///
/// # Errors
/// Static namespace/schema/default/path validation failure.
pub fn aws_bedrock_settings_definition()
-> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let defaults = serde_json::json!({
        "prompt_cache":{"mode":"disabled","points":[]},
        "guardrail":{
            "mode":"disabled",
            "identifier":"",
            "version":"",
            "trace":"provider-default",
            "stream_mode":"provider-default"
        }
    });
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "required":["prompt_cache","guardrail"],
            "properties":{
                "prompt_cache":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["mode","points"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "points":{
                            "type":"array",
                            "maxItems":3,
                            "uniqueItems":true,
                            "items":{
                                "type":"object",
                                "additionalProperties":false,
                                "required":["placement","ttl"],
                                "properties":{
                                    "placement":{"type":"string","enum":[
                                        "tools","system","latest-user-message"
                                    ]},
                                    "ttl":{"type":"string","enum":["5m","1h"]}
                                }
                            }
                        }
                    }
                },
                "guardrail":{
                    "type":"object",
                    "additionalProperties":false,
                    "required":["mode","identifier","version","trace","stream_mode"],
                    "properties":{
                        "mode":{"type":"string","enum":["disabled","enabled"]},
                        "identifier":{"type":"string","maxLength":2048},
                        "version":{"type":"string","maxLength":8},
                        "trace":{"type":"string","enum":[
                            "provider-default","enabled","disabled","enabled-full"
                        ]},
                        "stream_mode":{"type":"string","enum":[
                            "provider-default","sync","async"
                        ]}
                    }
                }
            }
        }),
        defaults,
        |value| {
            AwsBedrockSettings::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_public_path(SettingsFieldPath::new("prompt_cache")?)
    .with_public_path(SettingsFieldPath::new("guardrail")?)
    .with_wire_exposure();
    Ok(
        SettingsDefinition::new(aws_bedrock_settings_namespace()?, schema)
            .with_applies(SettingsApplies::Restart),
    )
}

/// Resolve the registered PAWS06 policy.
///
/// # Errors
/// Missing/poisoned Settings state or invalid resolved policy fails closed.
pub fn resolve_aws_bedrock_settings(
    settings: &SettingsService,
) -> Result<AwsBedrockSettings, AwsBedrockSettingsError> {
    let namespace =
        aws_bedrock_settings_namespace().map_err(|_| AwsBedrockSettingsError::Unavailable)?;
    let snapshot = settings
        .get(&namespace)
        .map_err(|_| AwsBedrockSettingsError::Unavailable)?
        .ok_or(AwsBedrockSettingsError::Unavailable)?;
    AwsBedrockSettings::from_snapshot(&snapshot)
}

/// Resolve Settings and construct one supplied-evidence Converse config.
///
/// Root supplies route identity, credential reference, and exact model/cache
/// evidence; provider-specific cache/guardrail wire values remain here.
///
/// # Errors
/// Settings/evidence or inference-config validation failure.
pub fn aws_converse_config_from_settings(
    settings: &SettingsService,
    region: AwsRegion,
    credential: CredentialQuery,
    default_model: impl Into<String>,
    evidence: BedrockConverseModelEvidence,
    cache_capabilities: Option<BedrockPromptCacheCapabilities>,
) -> Result<AwsInferencePluginConfig, AwsBedrockSettingsError> {
    let default_model = default_model.into();
    let metadata = resolve_aws_bedrock_settings(settings)?
        .runtime_metadata(&default_model, cache_capabilities)?;
    AwsInferencePluginConfig::converse(region, credential, default_model, evidence, Some(metadata))
        .map_err(map_inference_error)
}

/// Resolve Settings and construct one lazy live-evidence Converse config.
///
/// The plugin still requires live streaming/on-demand catalog evidence before
/// a request. A configured cache additionally retains its separate evidence;
/// Settings never manufacture it.
///
/// # Errors
/// Settings/evidence or inference-config validation failure.
pub fn aws_converse_live_config_from_settings(
    settings: &SettingsService,
    region: AwsRegion,
    credential: CredentialQuery,
    default_model: impl Into<String>,
    cache_capabilities: Option<BedrockPromptCacheCapabilities>,
) -> Result<AwsInferencePluginConfig, AwsBedrockSettingsError> {
    let default_model = default_model.into();
    let metadata = resolve_aws_bedrock_settings(settings)?
        .runtime_metadata(&default_model, cache_capabilities)?;
    AwsInferencePluginConfig::converse_live(region, credential, default_model, Some(metadata))
        .map_err(map_inference_error)
}

/// Build a live-evidenced Converse plugin whose PAWS06 policy resolves from
/// the already-composed Settings service during activation.
///
/// The wrapper retains the provider plugin's exact static inventory while
/// deferring only policy resolution. This avoids a second settings snapshot
/// and the resulting composition-time TOCTOU window.
///
/// # Errors
/// Route, credential, model, or cache-evidence admission failure.
pub fn aws_converse_live_settings_plugin(
    region: AwsRegion,
    credential: CredentialQuery,
    default_model: impl Into<String>,
    cache_capabilities: Option<BedrockPromptCacheCapabilities>,
) -> Result<Box<dyn Plugin>, AwsBedrockSettingsError> {
    let default_model = default_model.into();
    let prototype = aws_inference_plugin(
        AwsInferencePluginConfig::converse_live(
            region.clone(),
            credential.clone(),
            default_model.clone(),
            None,
        )
        .map_err(map_inference_error)?,
    );
    Ok(Box::new(AwsConverseLiveSettingsPlugin {
        region,
        credential,
        default_model,
        cache_capabilities,
        descriptor: prototype.descriptor(),
        inventory: prototype.inventory(),
    }))
}

struct AwsConverseLiveSettingsPlugin {
    region: AwsRegion,
    credential: CredentialQuery,
    default_model: String,
    cache_capabilities: Option<BedrockPromptCacheCapabilities>,
    descriptor: PluginDescriptor,
    inventory: Vec<PluginContributionSpec>,
}

impl Plugin for AwsConverseLiveSettingsPlugin {
    fn name(&self) -> &'static str {
        "inference-bedrock-converse"
    }

    fn descriptor(&self) -> PluginDescriptor {
        self.descriptor
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        self.inventory.clone()
    }

    fn inject(&self) -> &'static [ServiceKey] {
        &[
            heycode_settings::SERVICE_SETTINGS,
            heycode_llm::SERVICE_PROVIDERS,
            heycode_llm::SERVICE_MODELS,
            heycode_http::SERVICE_HTTP,
            heycode_credentials::SERVICE_CREDENTIALS,
        ]
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let settings = context
            .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
            .ok_or_else(|| CoreError::other("Amazon Bedrock settings service is unavailable"))?;
        let config = aws_converse_live_config_from_settings(
            settings.as_ref(),
            self.region.clone(),
            self.credential.clone(),
            self.default_model.clone(),
            self.cache_capabilities.clone(),
        )
        .map_err(|error| CoreError::other(error.to_string()))?;
        aws_inference_plugin(config).apply(context)
    }
}

/// Register the PAWS06 namespace as a Context effect.
#[must_use]
pub fn aws_bedrock_settings_plugin() -> Box<dyn Plugin> {
    struct AwsBedrockSettingsPlugin;

    impl Plugin for AwsBedrockSettingsPlugin {
        fn name(&self) -> &'static str {
            "settings-aws-bedrock"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                AWS_BEDROCK_SETTINGS_NAMESPACE,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            settings
                .register(
                    context,
                    aws_bedrock_settings_definition().map_err(|_| {
                        CoreError::other("Amazon Bedrock settings schema is invalid")
                    })?,
                )
                .map(|_| ())
                .map_err(|_| CoreError::other("Amazon Bedrock settings are invalid"))
        }
    }

    Box::new(AwsBedrockSettingsPlugin)
}
