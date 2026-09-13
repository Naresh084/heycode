//! Persisted effective route selection and its human command plugin.

mod commands;
mod error;
mod fallback;
mod model;
mod plugin;
mod service;

pub use error::RoutingError;
pub use model::{
    RoutingOverrides, RoutingSelection, has_persisted_connection, requested_connection,
    requested_runtime, requires_setup, routing_definition, routing_definition_with_overrides,
    settings_namespace,
};
pub use plugin::{
    routing_auth_plugin, routing_plugin, routing_plugin_with_connections,
    routing_plugin_with_overrides,
};
pub use service::{
    ActiveRoutingConfiguration, AppliedDelegatedConfiguration, BackendEffortCatalog,
    DelegatedRuntimeControls, ModelControlChoice, ProviderSwitchOutcome, RoutingService,
    SelectionScope,
};

/// Settings-CAS-backed effective route selection service.
pub const SERVICE_ROUTING: heycode_core::ServiceKey = heycode_core::ServiceKey::new("routing");
