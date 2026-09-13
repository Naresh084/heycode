//! File catalog boundary failures.

/// Versioned catalog-cache path, parse, cancellation and I/O failures.
#[derive(Debug, thiserror::Error)]
pub enum FileCatalogError {
    /// File operation failed.
    #[error("catalog cache {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// JSON or schema shape was malformed.
    #[error("invalid catalog cache {path}: {message}")]
    Parse {
        /// Path involved.
        path: String,
        /// Safe parse detail.
        message: String,
    },
    /// Existing path was a symbolic link.
    #[error("catalog cache path {path} is a symbolic link; use an owner-only regular path")]
    SymbolicLink {
        /// Rejected path.
        path: String,
    },
    /// Existing path had the wrong file kind.
    #[error("catalog cache path {path} is not {expected}")]
    WrongFileType {
        /// Rejected path.
        path: String,
        /// Expected kind.
        expected: &'static str,
    },
    /// Document schema is newer than this binary.
    #[error("catalog cache {path} uses newer schema {found}; this heycode supports {supported}")]
    NewerSchema {
        /// Path involved.
        path: String,
        /// Version found.
        found: u32,
        /// Highest supported version.
        supported: u32,
    },
    /// Document schema is older or missing and has no migration.
    #[error("catalog cache {path} uses unsupported schema {found}; expected {supported}")]
    OlderSchema {
        /// Path involved.
        path: String,
        /// Version found, with zero representing a missing marker.
        found: u32,
        /// Current supported version.
        supported: u32,
    },
    /// Existing document exceeded the defensive read cap.
    #[error("catalog cache {path} exceeds the {max_bytes}-byte limit")]
    TooLarge {
        /// Path involved.
        path: String,
        /// Maximum accepted bytes.
        max_bytes: u64,
    },
    /// Save was cancelled before the atomic commit point.
    #[error("catalog cache save cancelled before commit")]
    Cancelled,
    /// Writer mutex was poisoned.
    #[error("catalog cache writer is unavailable after a previous panic")]
    WriterUnavailable,
}

impl FileCatalogError {
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    pub(crate) fn parse(path: &std::path::Path, message: impl Into<String>) -> Self {
        Self::Parse {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}

/// User catalog-override document path, parse and validation failures.
///
/// Every variant fails the whole load: a partially applied override set would
/// leave a user believing an assertion applied when it did not.
#[derive(Debug, thiserror::Error)]
pub enum CatalogOverrideError {
    /// Host clock could not supply a non-zero Unix-millisecond capture time.
    #[error("catalog override capture clock is unavailable")]
    ClockUnavailable,
    /// A host supplied zero as the capture instant.
    #[error("catalog override generation has no capture instant")]
    MissingCaptureInstant,
    /// File operation failed.
    #[error("catalog overrides {path}: {source}")]
    Io {
        /// Path involved.
        path: String,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// TOML or schema shape was malformed.
    #[error("invalid catalog overrides {path}: {message}")]
    Parse {
        /// Path involved.
        path: String,
        /// Safe parse detail.
        message: String,
    },
    /// Existing path was a symbolic link.
    #[error("catalog overrides path {path} is a symbolic link; use a regular file")]
    SymbolicLink {
        /// Rejected path.
        path: String,
    },
    /// Existing path had the wrong file kind.
    #[error("catalog overrides path {path} is not {expected}")]
    WrongFileType {
        /// Rejected path.
        path: String,
        /// Expected kind.
        expected: &'static str,
    },
    /// Document schema is newer than this binary.
    #[error("catalog overrides {path} use newer schema {found}; this heycode supports {supported}")]
    NewerSchema {
        /// Path involved.
        path: String,
        /// Version found.
        found: u32,
        /// Highest supported version.
        supported: u32,
    },
    /// Document schema is older or missing and has no migration.
    #[error("catalog overrides {path} use unsupported schema {found}; expected {supported}")]
    OlderSchema {
        /// Path involved.
        path: String,
        /// Version found, with zero representing a missing marker.
        found: u32,
        /// Current supported version.
        supported: u32,
    },
    /// Document exceeded the defensive read cap.
    #[error("catalog overrides {path} exceed the {max_bytes}-byte limit")]
    TooLarge {
        /// Path involved.
        path: String,
        /// Maximum accepted bytes.
        max_bytes: u64,
    },
    /// A provider or model id was blank.
    #[error("catalog overrides {path}: `{field}` must not be blank")]
    BlankIdentity {
        /// Path involved.
        path: String,
        /// Offending key.
        field: &'static str,
    },
    /// A capability value was not an exact tri-state name.
    #[error(
        "catalog overrides {path}: capability `{capability}` for `{provider}`/`{model}` is `{value}`; expected supported, unsupported or unknown"
    )]
    UnknownSupport {
        /// Path involved.
        path: String,
        /// Capability key.
        capability: &'static str,
        /// Provider named by the entry.
        provider: String,
        /// Model named by the entry.
        model: String,
        /// Rejected value exactly as written.
        value: String,
    },
    /// A numeric limit was zero, which asserts nothing usable.
    #[error(
        "catalog overrides {path}: `{field}` for `{provider}`/`{model}` must be greater than zero"
    )]
    ZeroLimit {
        /// Path involved.
        path: String,
        /// Offending key.
        field: &'static str,
        /// Provider named by the entry.
        provider: String,
        /// Model named by the entry.
        model: String,
    },
    /// One document named the same provider/model twice, so which assertion
    /// wins would be positional rather than declared.
    #[error("catalog overrides {path}: `{provider}`/`{model}` is overridden more than once")]
    DuplicateModel {
        /// Path involved.
        path: String,
        /// Contested provider.
        provider: String,
        /// Contested model.
        model: String,
    },
    /// An entry asserted nothing, which is a typo rather than an instruction.
    #[error("catalog overrides {path}: `{provider}`/`{model}` asserts no field")]
    EmptyOverride {
        /// Path involved.
        path: String,
        /// Provider named by the entry.
        provider: String,
        /// Model named by the entry.
        model: String,
    },
    /// Two layers used the same label, so an assertion could not name where it
    /// came from unambiguously.
    #[error("catalog override layer label `{label}` is used more than once")]
    DuplicateLayerLabel {
        /// Contested label.
        label: String,
    },
    /// A layer label was blank, leaving an assertion with no nameable origin.
    #[error("catalog override layer label must not be blank")]
    BlankLayerLabel,
}

impl CatalogOverrideError {
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    pub(crate) fn parse(path: &std::path::Path, message: impl Into<String>) -> Self {
        Self::Parse {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}
