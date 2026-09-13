//! Provider-owned setup metadata independent of an active inference route.

use crate::{ModelDescriptor, ProviderDescriptor, ProviderProfile};

/// Human connection category; cloud accounts belong to the provider category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFamily {
    /// Hosted API, router or cloud platform.
    Provider,
    /// Managed cloud platform with explicit non-secret coordinates.
    Cloud,
    /// Local or self-hosted model service.
    Local,
}

/// Evidence policy for models shown while creating one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionModelSelection {
    /// Use selectable catalog rows or the provider-owned default.
    Catalog,
    /// Show only catalog rows with exact tool-use support evidence.
    ToolCapableCatalog,
    /// Use catalog rows regardless of unknown capabilities and permit an exact
    /// user-entered model id when discovery is unavailable or incomplete.
    CatalogOrExplicit,
}

/// One provider-owned non-secret connection coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionParameter {
    /// Stable routing-coordinate id persisted with the selected connection.
    pub id: String,
    /// Human label shown beside the input.
    pub label: String,
    /// Provider-owned help for the expected value.
    pub description: String,
}

/// An available connection without an invented model default or inferred authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionProfile {
    /// Stable inference identity activated after selection.
    pub registry_name: String,
    /// Provider-owned display name and protocols.
    pub descriptor: ProviderDescriptor,
    /// Explicit provider fallback; local libraries generally have none.
    pub default_model: Option<String>,
    /// Optional credential reference, never a value.
    pub credential_reference: Option<String>,
    /// Connection picker category.
    pub family: ConnectionFamily,
    /// Documented endpoint shown before discovery.
    pub default_endpoint: Option<String>,
    /// Provider-owned external setup/recovery instructions.
    pub help: Option<String>,
    /// Exact model ids admitted by the composed adapter when its coverage is narrower
    /// than discovery. `None` leaves admission to the provider catalog/default.
    pub selectable_models: Option<Vec<String>>,
    /// Ordered non-secret coordinates collected before model discovery.
    pub parameters: Vec<ConnectionParameter>,
    /// Model evidence and explicit-entry policy for this connection.
    pub model_selection: ConnectionModelSelection,
}

impl ConnectionProfile {
    /// Whether this model fits the adapter's explicit scope, independently of
    /// availability or capability evidence in the catalog.
    #[must_use]
    pub fn admits_model(&self, model: &str) -> bool {
        self.selectable_models
            .as_ref()
            .is_none_or(|models| models.iter().any(|id| id == model))
    }

    /// Whether one discovered row is eligible for this connection picker.
    ///
    /// Capability evidence is consumed but never rewritten: an explicit
    /// generic-server policy can accept an Unknown row without turning it into
    /// Supported.
    #[must_use]
    pub fn admits_discovered_model(&self, model: &ModelDescriptor) -> bool {
        self.admits_model(&model.id)
            && match self.model_selection {
                ConnectionModelSelection::Catalog | ConnectionModelSelection::CatalogOrExplicit => {
                    true
                }
                ConnectionModelSelection::ToolCapableCatalog => {
                    model.capabilities.tools.is_supported()
                }
            }
    }

    /// Whether this connection explicitly permits a model absent from discovery.
    #[must_use]
    pub const fn allows_explicit_model(&self) -> bool {
        matches!(
            self.model_selection,
            ConnectionModelSelection::CatalogOrExplicit
        )
    }
}

impl From<ProviderProfile> for ConnectionProfile {
    fn from(profile: ProviderProfile) -> Self {
        Self {
            registry_name: profile.registry_name,
            descriptor: profile.descriptor,
            default_model: Some(profile.default_model),
            credential_reference: profile.credential_reference,
            family: ConnectionFamily::Provider,
            default_endpoint: None,
            help: None,
            selectable_models: None,
            parameters: Vec::new(),
            model_selection: ConnectionModelSelection::Catalog,
        }
    }
}
