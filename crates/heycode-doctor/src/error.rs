//! Doctor registry and schema failures.

/// Doctor registry/schema failure with secret-free fields only.
#[derive(Debug, thiserror::Error)]
pub enum DoctorError {
    /// A check id or result code violated the closed diagnostic identifier grammar.
    #[error("invalid doctor {kind}; expected lowercase ASCII segments separated by `.` or `-`")]
    InvalidIdentifier {
        /// Boundary being validated.
        kind: &'static str,
    },
    /// A static summary/repair string violated the display contract.
    #[error("invalid static doctor {kind}; text must be non-empty, trimmed and control-free")]
    InvalidStaticText {
        /// Boundary being validated.
        kind: &'static str,
    },
    /// Two live plugins claimed one check id.
    #[error("doctor check `{id}` is already registered")]
    DuplicateCheck {
        /// Validated safe id.
        id: String,
    },
    /// Internal state was poisoned by a prior panic.
    #[error("doctor registry is unavailable")]
    RegistryUnavailable,
}
