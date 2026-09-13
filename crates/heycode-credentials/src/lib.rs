//! Credential references, safe records, provider registry, and secret values.

mod doctor;
mod error;
mod model;
mod plugin;
mod provider;
mod resolution;
mod secret;
mod service;
mod settings;

pub use doctor::credentials_doctor_plugin;
pub use error::CredentialsError;
pub use model::{
    CredentialDescriptor, CredentialKind, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSource, CredentialValidation,
};
pub use plugin::credentials_plugin;
pub use provider::CredentialProvider;
pub use resolution::CredentialResolutionError;
pub use secret::CredentialSecret;
pub use service::CredentialsService;
pub use settings::settings_namespace;

/// Credential provider registry service.
pub const SERVICE_CREDENTIALS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("credentials");
