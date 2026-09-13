//! UI-neutral panel, dialog and status contribution registry.

mod error;
pub mod keymap;
mod model;
mod plugin;
pub mod preferences;
mod registry;
pub mod settings_ui;
pub mod terminal;
pub mod theme;

pub use error::UiRegistryError;
pub use model::{UiContributionDescriptor, UiContributionId, UiSlot};
pub use plugin::ui_registry_plugin;
pub use registry::{ThemeRegistration, UiRegistry};

/// UI contribution registry service.
pub const SERVICE_UI: heycode_core::ServiceKey = heycode_core::ServiceKey::new("ui");
/// Settings-surface contribution registry service.
pub const SERVICE_SETTINGS_UI: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("settings-ui");
