//! DeepSeek Harness SDK JSON-RPC delegated runtime plugin.

mod config;
mod plugin;
mod process;
mod protocol;
mod runtime;

pub use config::DeepSeekHarnessRuntimeConfig;
pub use plugin::deepseek_harness_runtime_plugin;
pub use protocol::PINNED_DSH_SESSION_EVENT_TYPES;
pub use runtime::{DeepSeekHarnessRuntime, deepseek_harness_runtime_descriptor};

/// Stable delegated runtime id.
pub const DEEPSEEK_HARNESS_RUNTIME_ID: &str = "deepseek-harness";

/// Exact pre-release SDK server version reviewed by this bridge.
pub const PINNED_DSH_SDK_VERSION: &str = "0.0.1";

/// Wire-stable server identity from the SDK protocol.
pub const DSH_SDK_SERVER_NAME: &str = "deepseek-harness-sdk-runtime";
