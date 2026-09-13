//! Stable routing/command failures.

/// Effective route discovery, validation, persistence or authorization failure.
#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    /// Settings namespace construction/registration/read/write failed.
    #[error("routing settings failed: {0}")]
    Settings(String),
    /// Explicit logout requires a new connection before inference can resume.
    #[error("no inference route is connected; finish setup first")]
    SetupRequired,
    /// Persisted/effective route shape violated the owner contract.
    #[error("invalid routing selection: {0}")]
    InvalidSelection(&'static str),
    /// Provider id is not registered.
    #[error("unknown inference provider `{0}`")]
    UnknownProvider(String),
    /// Runtime id is not registered or cannot be activated here.
    #[error("unknown or unavailable agent runtime `{0}`")]
    UnknownRuntime(String),
    /// Model lacks current catalog evidence.
    #[error("model `{model}` is not selectable for provider `{provider}`; open /model and refresh")]
    UnselectableModel {
        /// Effective provider id.
        provider: String,
        /// Rejected model id.
        model: String,
    },
    /// A route change crosses provider-native opaque compaction state.
    #[error(
        "provider switch to `{provider}`/`{model}` requires an explicit opaque-state choice: portable, fork, or cancel"
    )]
    OpaqueStateResolutionRequired {
        /// Target provider id.
        provider: String,
        /// Target provider-owned default/canonical model.
        model: String,
    },
    /// Agent-side portable/fork preparation failed safely.
    #[error("provider switch opaque-state preparation failed")]
    OpaqueStatePreparation,
    /// Reasoning effort has no active adapter-owned vocabulary.
    #[error("the active route does not expose live reasoning effort controls")]
    EffortUnavailable,
    /// Live delegated discovery or configuration failed with a safe diagnostic.
    #[error("delegated runtime control failed: {0}")]
    BackendControl(String),
    /// A live update succeeded but could not be reconciled with durable routing.
    #[error(
        "the delegated runtime session was retired because its configuration could not be committed; reopen the session"
    )]
    BackendSessionRetired,
    /// Connect target is not a unique contributed flow/provider.
    #[error("unknown or ambiguous connect target `{0}`")]
    UnknownConnectTarget(String),
    /// Provider/runtime has no removable credential metadata.
    #[error("route `{0}` has no removable credential owned by an installed authorization flow")]
    NoLogoutTarget(String),
    /// Authorization failed with already-redacted text.
    #[error("authorization failed: {0}")]
    Authorization(String),
    /// Credential deletion failed with already-redacted text.
    #[error("logout failed: {0}")]
    Credential(String),
    /// Registry state could not be read.
    #[error("routing registry is unavailable")]
    RegistryUnavailable,
}
