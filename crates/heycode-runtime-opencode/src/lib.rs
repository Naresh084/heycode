//! OpenCode ACP delegated runtime plugin over the common process service.

mod config;
mod plugin;

pub use config::{OpenCodeRuntimeConfig, opencode_environment_snapshot};
pub use plugin::opencode_runtime_plugin;

/// Stable delegated runtime id.
pub const OPENCODE_RUNTIME_ID: &str = "opencode";

/// Exact OpenCode release reviewed for this ACP bridge.
pub const PINNED_OPENCODE_VERSION: &str = "1.18.21";

/// Current official OpenCode Go GLM-5.3-Flash selection identity.
///
/// OpenCode documents full model selections as `provider/model-id` and names
/// this route `opencode-go/glm-5.3-flash`. The retired anonymous Ox preview
/// identity is deliberately not accepted as the live-canary target.
pub const OPENCODE_GLM_5_3_FLASH_MODEL_ID: &str = "opencode-go/glm-5.3-flash";
