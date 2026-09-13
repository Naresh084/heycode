//! Explicit product policy for bridge-complete zero-configuration hosted tools.

use heycode_core::{
    Context, CoreError, CoreResult, NativeToolImplementationKind, Plugin, PluginContributionKind,
    PluginDescriptor,
};
use heycode_llm::LlmError;

use crate::catalog::OPENAI_PROVIDER;
use crate::{OpenAiHostedToolDefinition, OpenAiHostedToolKind, OpenAiProvider};

/// Exact hosted-tool families that need no external identifier, server,
/// client-action, media-admission or approval input and whose shared bridge is
/// complete.
pub const OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS: [OpenAiHostedToolKind; 3] = [
    OpenAiHostedToolKind::WebSearch,
    OpenAiHostedToolKind::CodeInterpreter,
    OpenAiHostedToolKind::HostedShell,
];

/// Explicit OpenAI hosted-tool product policy.
///
/// This type intentionally has no `Default`: enabling provider-executed work
/// is a named product decision. The sole current constructor contains only
/// bridge-complete definitions that require no deployment-specific values.
#[derive(Clone)]
pub struct OpenAiHostedToolProductPolicy {
    definitions: Vec<OpenAiHostedToolDefinition>,
}

impl OpenAiHostedToolProductPolicy {
    /// Construct the exact bridge-complete, zero-configuration policy.
    #[must_use]
    pub fn bridge_complete_zero_configuration() -> Self {
        Self {
            definitions: vec![
                OpenAiHostedToolDefinition::web_search(),
                OpenAiHostedToolDefinition::code_interpreter(),
                OpenAiHostedToolDefinition::hosted_shell(),
            ],
        }
    }

    /// Exact admitted kinds in wire-definition order.
    #[must_use]
    pub fn kinds(&self) -> Vec<OpenAiHostedToolKind> {
        self.definitions
            .iter()
            .map(OpenAiHostedToolDefinition::kind)
            .collect()
    }

    /// Consume the named baseline so configured policy can extend the same
    /// literal definition generation without reconstructing it.
    pub(crate) fn into_definitions(self) -> Vec<OpenAiHostedToolDefinition> {
        self.definitions
    }

    /// Apply this explicit policy to one OpenAI Provider.
    ///
    /// # Errors
    /// Provider/model capability drift or shared-plan construction failure is
    /// returned before the Provider is published.
    pub fn configure(self, provider: OpenAiProvider) -> Result<OpenAiProvider, LlmError> {
        provider.with_hosted_tools(self.definitions)
    }
}

impl std::fmt::Debug for OpenAiHostedToolProductPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiHostedToolProductPolicy")
            .field("kinds", &self.kinds())
            .finish()
    }
}

/// Configure one Provider with the explicit bridge-complete,
/// zero-configuration hosted-tool policy.
///
/// # Errors
/// Same contract as [`OpenAiHostedToolProductPolicy::configure`].
pub fn configure_openai_bridge_complete_hosted_tools(
    provider: OpenAiProvider,
) -> Result<OpenAiProvider, LlmError> {
    OpenAiHostedToolProductPolicy::bridge_complete_zero_configuration().configure(provider)
}

/// Register exactly the N01 candidates consumed by the bridge-complete policy.
///
/// Each row is registered through `NativeToolRegistry::register`, which makes
/// it a Context effect. Partial activation failure and Context shutdown unwind
/// the rows LIFO.
#[must_use]
pub fn openai_bridge_complete_native_tools_plugin() -> Box<dyn Plugin> {
    struct OpenAiBridgeCompleteNativeToolsPlugin;

    impl Plugin for OpenAiBridgeCompleteNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-openai"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS
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
            for kind in OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS {
                let implementation = heycode_native_tools::NativeToolImplementation::new(
                    kind.as_str(),
                    kind.implementation_id(),
                    NativeToolImplementationKind::Provider,
                    Some(OPENAI_PROVIDER.to_owned()),
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

    Box::new(OpenAiBridgeCompleteNativeToolsPlugin)
}
