//! Settings-backed native/local routing policy contribution.

use std::collections::BTreeMap;

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_settings::{SettingsDefinition, SettingsNamespace, SettingsSchema};

use crate::{NativeToolRegistry, SERVICE_NATIVE_TOOLS};

/// Selection mode for one logical tool capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeToolPolicyMode {
    /// Prefer matching provider execution, falling back to client/MCP.
    PreferNative,
    /// Prefer client/MCP execution, falling back to matching provider.
    PreferLocal,
    /// Require matching provider execution.
    NativeOnly,
    /// Require client/MCP execution.
    LocalOnly,
}

impl NativeToolPolicyMode {
    /// Stable settings value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreferNative => "prefer-native",
            Self::PreferLocal => "prefer-local",
            Self::NativeOnly => "native-only",
            Self::LocalOnly => "local-only",
        }
    }

    pub(crate) const fn is_only(self) -> bool {
        matches!(self, Self::NativeOnly | Self::LocalOnly)
    }

    fn parse(value: &str) -> Result<Self, NativeToolPolicyError> {
        match value {
            "prefer-native" => Ok(Self::PreferNative),
            "prefer-local" => Ok(Self::PreferLocal),
            "native-only" => Ok(Self::NativeOnly),
            "local-only" => Ok(Self::LocalOnly),
            _ => Err(NativeToolPolicyError::InvalidMode),
        }
    }
}

impl std::fmt::Display for NativeToolPolicyMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Immutable default plus per-logical native-tool selection policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeToolPolicy {
    default: NativeToolPolicyMode,
    overrides: BTreeMap<String, NativeToolPolicyMode>,
}

impl NativeToolPolicy {
    /// Construct one validated policy.
    ///
    /// # Errors
    /// Unsafe logical ids or more than 128 overrides fail.
    pub fn new(
        default: NativeToolPolicyMode,
        overrides: BTreeMap<String, NativeToolPolicyMode>,
    ) -> Result<Self, NativeToolPolicyError> {
        if overrides.len() > 128 || overrides.keys().any(|logical| !safe_logical(logical)) {
            return Err(NativeToolPolicyError::InvalidLogical);
        }
        Ok(Self { default, overrides })
    }

    /// Effective mode for one logical capability.
    #[must_use]
    pub fn mode_for(&self, logical: &str) -> NativeToolPolicyMode {
        self.overrides.get(logical).copied().unwrap_or(self.default)
    }

    /// Per-logical override map.
    #[must_use]
    pub fn overrides(&self) -> &BTreeMap<String, NativeToolPolicyMode> {
        &self.overrides
    }

    pub(crate) fn from_value(value: &serde_json::Value) -> Result<Self, NativeToolPolicyError> {
        let object = value
            .as_object()
            .ok_or(NativeToolPolicyError::InvalidShape)?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "default" | "overrides"))
        {
            return Err(NativeToolPolicyError::InvalidShape);
        }
        let default = object
            .get("default")
            .and_then(serde_json::Value::as_str)
            .ok_or(NativeToolPolicyError::InvalidShape)
            .and_then(NativeToolPolicyMode::parse)?;
        let overrides = object
            .get("overrides")
            .and_then(serde_json::Value::as_object)
            .ok_or(NativeToolPolicyError::InvalidShape)?
            .iter()
            .map(|(logical, mode)| {
                let mode = mode
                    .as_str()
                    .ok_or(NativeToolPolicyError::InvalidMode)
                    .and_then(NativeToolPolicyMode::parse)?;
                Ok((logical.clone(), mode))
            })
            .collect::<Result<BTreeMap<_, _>, NativeToolPolicyError>>()?;
        Self::new(default, overrides)
    }
}

impl Default for NativeToolPolicy {
    fn default() -> Self {
        Self {
            default: NativeToolPolicyMode::PreferNative,
            overrides: BTreeMap::new(),
        }
    }
}

/// Native-tool policy validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeToolPolicyError {
    /// Top-level settings shape/keys are invalid.
    #[error("native tool policy shape is invalid")]
    InvalidShape,
    /// One policy mode is unknown.
    #[error("native tool policy mode is invalid")]
    InvalidMode,
    /// One logical id or the override count is invalid.
    #[error("native tool policy logical override is invalid")]
    InvalidLogical,
}

/// Settings namespace owned by the policy plugin.
///
/// # Errors
/// Static namespace validation failure.
pub fn native_tool_policy_namespace() -> Result<SettingsNamespace, heycode_settings::SettingsError>
{
    SettingsNamespace::new("native-tools")
}

fn definition() -> Result<SettingsDefinition, heycode_settings::SettingsError> {
    let modes = ["prefer-native", "prefer-local", "native-only", "local-only"];
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type":"object",
            "additionalProperties":false,
            "properties":{
                "default":{"type":"string","enum":modes},
                "overrides":{
                    "type":"object",
                    "additionalProperties":{"type":"string","enum":modes}
                }
            }
        }),
        serde_json::json!({"default":"prefer-native","overrides":{}}),
        |value| {
            NativeToolPolicy::from_value(value)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )?
    .with_wire_exposure();
    Ok(SettingsDefinition::new(
        native_tool_policy_namespace()?,
        schema,
    ))
}

/// Mount live Settings-backed native/local selection policy.
#[must_use]
pub fn native_tool_policy_plugin() -> Box<dyn Plugin> {
    struct NativeToolPolicyPlugin;

    impl Plugin for NativeToolPolicyPlugin {
        fn name(&self) -> &'static str {
            "native-tool-policy"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "native-tools",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS, SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            let registry = context
                .get::<NativeToolRegistry>(SERVICE_NATIVE_TOOLS)
                .ok_or_else(|| CoreError::other("native-tools service type mismatch"))?;
            let namespace = native_tool_policy_namespace()
                .map_err(|error| CoreError::other(error.to_string()))?;
            let snapshot = settings
                .register(
                    context,
                    definition().map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            let initial = NativeToolPolicy::from_value(snapshot.resolved())
                .map_err(|error| CoreError::other(error.to_string()))?;
            registry
                .replace_policy(initial)
                .map_err(|error| CoreError::other(error.to_string()))?;
            let reset = registry.clone();
            context.effect(move || reset.reset_policy());
            let watcher = registry.clone();
            settings
                .watch(
                    context,
                    &namespace,
                    move |change| match NativeToolPolicy::from_value(change.next().resolved())
                        .map_err(|_| ())
                        .and_then(|policy| watcher.replace_policy(policy).map_err(|_| ()))
                    {
                        Ok(()) => {}
                        Err(()) => watcher.mark_policy_unavailable(),
                    },
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            Ok(())
        }
    }

    Box::new(NativeToolPolicyPlugin)
}

fn safe_logical(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}
