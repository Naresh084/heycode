//! Settings boundary and registration failures.

use crate::{SettingsLayer, WireExposureFault};

/// Failures raised while defining, layering, or registering settings.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Namespace was not lowercase kebab-case.
    #[error("settings namespace `{value}` must match [a-z][a-z0-9-]*")]
    InvalidNamespace {
        /// Rejected namespace.
        value: String,
    },
    /// Schema metadata was not an object.
    #[error("settings schema metadata must be a JSON object")]
    SchemaMustBeObject,
    /// Defaults failed structural or owner validation.
    #[error("invalid settings schema defaults: {message}")]
    InvalidDefaults {
        /// Validator or shape failure.
        message: String,
    },
    /// A mergeable section was not an object.
    #[error("settings namespace `{namespace}` {layer} layer must be a JSON object")]
    LayerMustBeObject {
        /// Namespace whose layer failed.
        namespace: String,
        /// Rejected layer.
        layer: SettingsLayer,
    },
    /// Fully resolved value failed the owner's validator.
    #[error("invalid resolved settings for `{namespace}`: {message}")]
    InvalidResolved {
        /// Namespace being registered.
        namespace: String,
        /// Owner validator message.
        message: String,
    },
    /// Namespace already has a live registration.
    #[error("settings namespace `{namespace}` is already registered")]
    DuplicateNamespace {
        /// Duplicate namespace.
        namespace: String,
    },
    /// Read-only settings service received a write.
    #[error("settings provider is read-only")]
    ReadOnly,
    /// Write named a namespace with no live owner registration.
    #[error("settings namespace `{namespace}` is not registered")]
    UnknownNamespace {
        /// Missing namespace.
        namespace: String,
    },
    /// A caller attempted to overwrite a newer raw user section.
    #[error(
        "settings namespace `{namespace}` changed since it was read (expected revision {expected}, now {actual})"
    )]
    Conflict {
        /// Namespace being written.
        namespace: String,
        /// Revision supplied by the caller.
        expected: u64,
        /// Current authoritative revision.
        actual: u64,
    },
    /// Any settings layer or namespace owner changed after the caller read it.
    #[error("settings namespace `{namespace}` changed since its snapshot was read")]
    StaleSnapshot {
        /// Namespace being written.
        namespace: String,
    },
    /// Watcher id counter exhausted.
    #[error("settings watcher id space exhausted")]
    WatcherIdExhausted,
    /// Raw-section revision counter exhausted.
    #[error("settings revision space exhausted for `{namespace}`")]
    RevisionExhausted {
        /// Namespace whose counter cannot advance.
        namespace: String,
    },
    /// A synchronous watcher attempted to re-enter the serialized write lane.
    #[error(
        "settings watcher callbacks cannot synchronously write; schedule the write after the callback returns"
    )]
    ReentrantWrite,
    /// Provider failed before a candidate could commit.
    #[error("settings provider failed: {message}")]
    Provider {
        /// Redacted provider error.
        message: String,
    },
    /// A declared field path was not a dot-separated segment list.
    #[error("settings field path `{value}` must be dot-separated non-empty segments")]
    InvalidFieldPath {
        /// Rejected path.
        value: String,
    },
    /// A namespace attested wire exposure it cannot prove.
    #[error(
        "settings namespace `{namespace}` cannot prove wire exposure at path `{path}`: {fault}"
    )]
    UnprovableWireExposure {
        /// Namespace being registered.
        namespace: String,
        /// Rendered path of the first unprovable position.
        path: String,
        /// Closed reason; it never carries the offending value.
        fault: WireExposureFault,
    },
    /// A user write named a path the administrator layer owns.
    #[error("settings namespace `{namespace}` path `{path}` is managed and cannot be written")]
    ManagedLock {
        /// Namespace being written.
        namespace: String,
        /// Managed leaf path the write would have shadowed.
        path: String,
    },
    /// Registry mutex was poisoned by a panicking caller.
    #[error("settings registry is unavailable after a previous panic")]
    RegistryUnavailable,
}
