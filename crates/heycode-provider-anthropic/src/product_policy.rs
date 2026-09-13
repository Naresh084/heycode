//! Explicit default-model product policy for atomic Anthropic server tools.

use heycode_core::{
    Context, CoreError, CoreResult, NativeToolImplementationKind, Plugin, PluginContributionKind,
    PluginDescriptor,
};
use heycode_llm::LlmError;

use crate::catalog::ANTHROPIC_PROVIDER;
use crate::{
    ANTHROPIC_CLAUDE_OPUS_5, AnthropicProvider, AnthropicServerToolDefinition,
    AnthropicServerToolFault, AnthropicServerToolKind, AnthropicServerToolPlan,
};

/// Exact zero-configuration families forming the maintained default model's
/// atomic server-tool product plan.
pub const ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS: [AnthropicServerToolKind; 2] = [
    AnthropicServerToolKind::WebSearch,
    AnthropicServerToolKind::CodeExecution,
];

/// Explicit atomic server-tool policy for the maintained Anthropic default.
///
/// This type intentionally has no `Default`: provider-executed work is enabled
/// only through the named constructor after exact model admission.
#[derive(Clone)]
pub struct AnthropicMaintainedDefaultServerToolPolicy {
    plan: AnthropicServerToolPlan,
}

impl AnthropicMaintainedDefaultServerToolPolicy {
    /// Construct and admit the exact zero-configuration atomic plan against
    /// the maintained default model.
    ///
    /// # Errors
    /// Definition drift, model-support drift or shared-plan construction
    /// failure is returned before a Provider or candidate plugin is published.
    pub fn atomic_zero_configuration() -> Result<Self, AnthropicServerToolFault> {
        let plan = AnthropicServerToolPlan::new(vec![
            AnthropicServerToolDefinition::web_search(),
            AnthropicServerToolDefinition::code_execution(),
        ])?;
        let admitted = plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5)?;
        drop(admitted);
        Ok(Self { plan })
    }

    /// Exact admitted kinds in atomic wire-definition order.
    #[must_use]
    pub fn kinds(&self) -> Vec<AnthropicServerToolKind> {
        self.plan
            .definitions()
            .iter()
            .map(AnthropicServerToolDefinition::kind)
            .collect()
    }

    /// Consume the named baseline so configured policy extends the identical
    /// admitted plan rather than reconstructing it.
    pub(crate) fn into_plan(self) -> AnthropicServerToolPlan {
        self.plan
    }

    /// Apply this exact atomic policy to one Anthropic Provider.
    ///
    /// # Errors
    /// Shared adapter construction or provider-option admission failure is
    /// returned before Provider publication.
    pub fn configure(self, provider: AnthropicProvider) -> Result<AnthropicProvider, LlmError> {
        provider.with_server_tools(self.plan)
    }
}

impl std::fmt::Debug for AnthropicMaintainedDefaultServerToolPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicMaintainedDefaultServerToolPolicy")
            .field("kinds", &self.kinds())
            .finish()
    }
}

/// Configure one Provider with the explicitly admitted maintained-default
/// atomic server-tool policy.
///
/// # Errors
/// Provider policy construction or adapter configuration failure is returned
/// before publication.
pub fn configure_anthropic_default_server_tools(
    provider: AnthropicProvider,
) -> Result<AnthropicProvider, LlmError> {
    let policy = AnthropicMaintainedDefaultServerToolPolicy::atomic_zero_configuration()
        .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;
    policy.configure(provider)
}

/// Register exactly the N01 candidates consumed by the maintained-default
/// atomic policy.
///
/// Each row is a Context effect. Partial activation failure and Context
/// shutdown unwind the rows LIFO.
#[must_use]
pub fn anthropic_default_native_tools_plugin() -> Box<dyn Plugin> {
    struct AnthropicDefaultNativeToolsPlugin;

    impl Plugin for AnthropicDefaultNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-anthropic"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS
                .iter()
                .map(|kind| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::NativeTool,
                        kind.implementation_id(),
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_native_tools::SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| CoreError::other("native-tools service type mismatch"))?;
            for kind in ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS {
                let implementation = heycode_native_tools::NativeToolImplementation::new(
                    kind.as_str(),
                    kind.implementation_id(),
                    NativeToolImplementationKind::Provider,
                    Some(ANTHROPIC_PROVIDER.to_owned()),
                    100,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
                registry
                    .register(context, implementation)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }

    Box::new(AnthropicDefaultNativeToolsPlugin)
}
