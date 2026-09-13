//! Format-preserving TOML provider for the heycode settings service.

mod config;
mod error;
mod plugin;
mod provider;

pub use config::FileSettingsConfig;
pub use error::FileSettingsError;
pub use plugin::{file_settings_plugin, file_settings_plugin_with_overlay};
pub use provider::{FileSettingsProvider, FileWatchHandle, PinnedSettingsOverlay};

/// Current standalone settings-file schema.
pub const FILE_SCHEMA_VERSION: u32 = 1;
