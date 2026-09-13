//! Layered settings Service Definition for heycode plugins.
//!
//! Namespace owners declare a schema, immutable defaults and an optional
//! composition base. A provider supplies user/project documents. Resolution
//! is deterministic: defaults → base → user → project → managed. Persistence,
//! writes, revisions and watchers are separate provider concerns.

mod doctor;
mod documents;
mod error;
mod model;
mod plugin;
mod redaction;
mod schema;
mod service;

pub use doctor::settings_doctor_plugin;
pub use documents::SettingsDocuments;
pub use error::SettingsError;
pub use model::{SettingsApplies, SettingsLayer, SettingsNamespace, SettingsUpdateSource};
pub use plugin::settings_plugin;
pub use redaction::{
    REDACTED_PLACEHOLDER, SettingsFieldPath, SettingsWireProjection, WireExposureFault,
    names_credential_material, screen_text_for_credentials,
};
pub use schema::{SettingsDefinition, SettingsSchema};
pub use service::{SettingsChange, SettingsService, SettingsSnapshot, SettingsWriter};

/// Layered settings registry service.
pub const SERVICE_SETTINGS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("settings");
