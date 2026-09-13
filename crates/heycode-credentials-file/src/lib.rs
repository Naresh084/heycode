//! Owner-only schema-v1 credential file storage and legacy migration.

mod config;
mod error;
mod plugin;
mod store;

pub use config::FileCredentialConfig;
pub use error::FileCredentialError;
pub use plugin::file_credentials_plugin;
pub use store::FileCredentialProvider;

/// Current credential-file schema.
pub const FILE_SCHEMA_VERSION: u32 = 1;
