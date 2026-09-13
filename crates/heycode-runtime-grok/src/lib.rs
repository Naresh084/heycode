//! Official Grok Build subscription runtime over its pinned ACP process.

mod config;
mod plugin;

pub use config::{GrokRuntimeConfig, grok_environment_snapshot};
pub use plugin::grok_runtime_plugin;

/// Stable Grok Build delegated runtime id.
pub const GROK_RUNTIME_ID: &str = "grok";
/// Reviewed Grok Build release.
pub const PINNED_GROK_VERSION: &str = "1.0.13";
/// Exact version output of the reviewed release artifact.
pub const PINNED_GROK_VERSION_OUTPUT: &str = "grok 1.0.13 (5e9a58528b76)";

/// Capabilities implemented through the official ACP protocol.
///
/// # Errors
/// Static descriptor validation failure.
pub fn grok_descriptor()
-> Result<heycode_runtime::AgentRuntimeDescriptor, heycode_runtime::RuntimeContractError> {
    use heycode_llm::CapabilitySupport::{Supported, Unknown, Unsupported};
    heycode_runtime::AgentRuntimeDescriptor::new(
        GROK_RUNTIME_ID,
        "Grok Build",
        heycode_runtime::AgentRuntimeKind::Delegated,
        heycode_runtime::RuntimeCapabilities {
            models: Supported,
            resume: Supported,
            fork: Unsupported,
            steer: Unsupported,
            follow_up: Unsupported,
            permissions: Supported,
            questions: Unsupported,
            compaction: Unsupported,
        },
    )?
    .with_configuration_capabilities(heycode_runtime::RuntimeConfigurationCapabilities {
        system_prompt: Unsupported,
        tools: Unsupported,
        model: Supported,
        // The ACP bridge applies this only when the live process advertises a
        // thought-level option; the pinned Grok descriptor does not claim it.
        reasoning_effort: Unknown,
    })
    .with_connection_help("Install Grok Build from docs.x.ai/build/cli. Sign in with `grok auth login --device-auth`, then return here and press Enter to retry.")
}
