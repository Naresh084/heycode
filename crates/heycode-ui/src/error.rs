//! UI descriptor/registry failures.

/// UI contribution boundary failure.
#[derive(Debug, thiserror::Error)]
pub enum UiRegistryError {
    /// Id grammar violation.
    #[error("invalid UI contribution id; expected lowercase kebab/dotted segments")]
    InvalidId,
    /// Title grammar/cap violation.
    #[error("invalid UI contribution title; expected trimmed control-free text")]
    InvalidTitle,
    /// One slot/id already has a live owner.
    #[error("UI contribution `{identity}` is already registered")]
    Duplicate {
        /// Safe `slot:id` identity.
        identity: String,
    },
    /// Context exact-inventory publication failed.
    #[error("UI contribution inventory failed: {message}")]
    Inventory {
        /// Safe core diagnostic.
        message: String,
    },
    /// Registry mutex was poisoned.
    #[error("UI contribution registry is unavailable")]
    RegistryUnavailable,
}
