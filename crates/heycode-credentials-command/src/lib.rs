//! Read-only command-backed credential provider.

mod error;
mod plugin;
mod provider;
mod spec;

pub use error::CommandCredentialError;
pub use plugin::command_credentials_plugin;
pub use provider::CommandCredentialProvider;
pub use spec::CommandCredentialSpec;
