//! Provider-owned bindings for documented Chat Completions services.
//!
//! Endpoint compatibility supplies a protocol, never intrinsic model evidence.

mod catalog;
mod inference;
mod plugin;
mod specs;

pub use catalog::CompatibleCatalog;
pub use inference::CompatibleProvider;
pub use plugin::{compatible_authorization_flows, compatible_catalog_plugin};
pub use specs::{CatalogDialect, CompatibleSpec, builtin_specs, spec};
