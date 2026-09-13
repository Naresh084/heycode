//! Owner-only versioned file persistence for committed model catalogs, plus
//! the layered user catalog overrides that stand beside them.
//!
//! The two documents are deliberately different files with different readers,
//! and that separation is the provenance guarantee. Bytes read from the cache
//! at `$HEYCODE_HOME/cache/models.json` can only ever become provider evidence;
//! bytes read from an override layer can only ever become a user assertion.
//! Neither reader can mint the other's provenance, and no document can spell
//! its own, so an assertion has no route to being read back as vendor fact.

mod attribution;
mod config;
mod error;
mod override_wire;
mod overrides;
mod plugin;
mod store;
mod wire;

pub use attribution::{
    AssertionDirection, AttributedCatalog, AttributedLimit, AttributedModel, AttributedSupport,
    CapabilityAssertion, CapabilityEnforcement, EvidencedLimit, EvidencedSupport, LimitAssertion,
    LimitEnforcement, ModelAssertion, ModelCapabilityKind, ModelLimitField, OverrideSource,
    UnmatchedOverride,
};
pub use config::FileCatalogConfig;
pub use error::{CatalogOverrideError, FileCatalogError};
pub use overrides::{
    CatalogOverrideLayer, CatalogOverrides, CatalogOverridesConfig, OVERRIDE_SCHEMA_VERSION,
};
pub use plugin::{catalog_overrides_plugin, file_catalog_persistence_plugin};
pub use store::FileCatalogPersistence;

/// Loaded layered user catalog overrides.
pub const SERVICE_CATALOG_OVERRIDES: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("catalog-overrides");

/// Current on-disk catalog-cache schema.
pub const FILE_SCHEMA_VERSION: u32 = 2;
/// Oldest catalog-cache schema this build can read.
pub const MIN_FILE_SCHEMA_VERSION: u32 = 1;
