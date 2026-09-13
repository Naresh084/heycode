//! Validated admission configuration, input, receipt and stable failures.

use std::path::{Component, Path, PathBuf};

const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

/// Explicit local attachment-store configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentStoreConfig {
    root: PathBuf,
    maximum_bytes: usize,
}

impl AttachmentStoreConfig {
    /// Construct one absolute owner-only store configuration.
    ///
    /// # Errors
    /// Relative/traversing/NUL root or a limit outside 1 byte..=64 MiB fails.
    pub fn new(
        root: impl Into<PathBuf>,
        maximum_bytes: usize,
    ) -> Result<Self, AttachmentStoreError> {
        let root = root.into();
        if !root.is_absolute()
            || root.as_os_str().is_empty()
            || root
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            || root.as_os_str().as_encoded_bytes().contains(&0)
            || !(1..=MAX_ATTACHMENT_BYTES).contains(&maximum_bytes)
        {
            return Err(AttachmentStoreError::invalid_input());
        }
        Ok(Self {
            root,
            maximum_bytes,
        })
    }

    /// Absolute store root. Expose only to the provider during composition.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Per-object byte ceiling.
    #[must_use]
    pub const fn maximum_bytes(&self) -> usize {
        self.maximum_bytes
    }
}

impl std::fmt::Debug for AttachmentStoreConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentStoreConfig")
            .field("root", &"<redacted>")
            .field("maximum_bytes", &self.maximum_bytes)
            .finish()
    }
}

/// One caller-supplied attachment candidate.
#[derive(Clone)]
pub struct AttachmentInput {
    bytes: Vec<u8>,
    claimed_media_type: Option<heycode_core::AttachmentMediaType>,
    display_name: Option<String>,
    source: Option<heycode_core::AttachmentSourceMetadata>,
}

impl AttachmentInput {
    /// Construct one bounded candidate without trusting its MIME claim.
    ///
    /// # Errors
    /// Empty/>64 MiB bytes or malformed claimed MIME fails. Display-name
    /// coherence is checked with the final durable metadata during admission.
    pub fn new(
        bytes: Vec<u8>,
        claimed_media_type: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<Self, AttachmentStoreError> {
        if bytes.is_empty() {
            return Err(AttachmentStoreError::invalid_input());
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentStoreError::size_limit());
        }
        let claimed_media_type = claimed_media_type
            .map(heycode_core::AttachmentMediaType::new)
            .transpose()
            .map_err(|_| AttachmentStoreError::invalid_input())?;
        Ok(Self {
            bytes,
            claimed_media_type,
            display_name: display_name.map(str::to_owned),
            source: None,
        })
    }

    /// Attach already-validated durable source provenance.
    #[must_use]
    pub fn with_source(mut self, source: heycode_core::AttachmentSourceMetadata) -> Self {
        self.source = Some(source);
        self
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn claimed_media_type(&self) -> Option<&heycode_core::AttachmentMediaType> {
        self.claimed_media_type.as_ref()
    }

    pub(crate) fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    pub(crate) fn source(&self) -> Option<&heycode_core::AttachmentSourceMetadata> {
        self.source.as_ref()
    }
}

impl std::fmt::Debug for AttachmentInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentInput")
            .field("byte_count", &self.bytes.len())
            .field("claimed_media_type", &self.claimed_media_type)
            .field("has_display_name", &self.display_name.is_some())
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

/// Successful durable attachment admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentAdmission {
    metadata: heycode_core::AttachmentMetadata,
    event_seq: u64,
}

impl AttachmentAdmission {
    pub(crate) const fn new(metadata: heycode_core::AttachmentMetadata, event_seq: u64) -> Self {
        Self {
            metadata,
            event_seq,
        }
    }

    /// Durable metadata.
    #[must_use]
    pub fn metadata(&self) -> &heycode_core::AttachmentMetadata {
        &self.metadata
    }

    /// Sequence of the committed `attachment/added` event.
    #[must_use]
    pub const fn event_seq(&self) -> u64 {
        self.event_seq
    }
}

/// Stable attachment-store failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentStoreErrorClass {
    /// Candidate shape/name is invalid.
    InvalidInput,
    /// Candidate exceeds a configured byte/dimension/pixel budget.
    SizeLimit,
    /// Claimed MIME disagrees with content inspection.
    MimeMismatch,
    /// Recognized media format is not currently admitted.
    UnsupportedMedia,
    /// Raster header/dimensions are invalid.
    InvalidImage,
    /// Caller cancelled before durable metadata commit.
    Cancelled,
    /// Context shutdown is terminal.
    Stopped,
    /// Storage or registry infrastructure is unavailable.
    Unavailable,
    /// Stable local storage I/O failure.
    Io,
    /// Immutable object/layout/hash/MIME state is corrupt.
    Corrupt,
    /// Durable session append failed.
    Session,
    /// Host lacks an audited owner-only storage backend.
    UnsupportedSecurity,
}

/// Body/path/content-free attachment-store error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AttachmentStoreError {
    class: AttachmentStoreErrorClass,
    message: &'static str,
}

impl AttachmentStoreError {
    const fn new(class: AttachmentStoreErrorClass, message: &'static str) -> Self {
        Self { class, message }
    }

    /// Stable class for UI/policy recovery.
    #[must_use]
    pub const fn class(&self) -> AttachmentStoreErrorClass {
        self.class
    }

    /// Construct the fixed safe error for one stable class.
    #[must_use]
    pub const fn from_class(class: AttachmentStoreErrorClass) -> Self {
        match class {
            AttachmentStoreErrorClass::InvalidInput => Self::invalid_input(),
            AttachmentStoreErrorClass::SizeLimit => Self::size_limit(),
            AttachmentStoreErrorClass::MimeMismatch => Self::mime_mismatch(),
            AttachmentStoreErrorClass::UnsupportedMedia => Self::unsupported_media(),
            AttachmentStoreErrorClass::InvalidImage => Self::invalid_image(),
            AttachmentStoreErrorClass::Cancelled => Self::cancelled(),
            AttachmentStoreErrorClass::Stopped => Self::stopped(),
            AttachmentStoreErrorClass::Unavailable => Self::unavailable(),
            AttachmentStoreErrorClass::Io => Self::io(),
            AttachmentStoreErrorClass::Corrupt => Self::corrupt(),
            AttachmentStoreErrorClass::Session => Self::session(),
            AttachmentStoreErrorClass::UnsupportedSecurity => Self::new(
                AttachmentStoreErrorClass::UnsupportedSecurity,
                "attachment owner-only security is unsupported on this host",
            ),
        }
    }

    pub(crate) const fn invalid_input() -> Self {
        Self::new(
            AttachmentStoreErrorClass::InvalidInput,
            "attachment input is invalid",
        )
    }

    pub(crate) const fn size_limit() -> Self {
        Self::new(
            AttachmentStoreErrorClass::SizeLimit,
            "attachment exceeds the configured limit",
        )
    }

    pub(crate) const fn mime_mismatch() -> Self {
        Self::new(
            AttachmentStoreErrorClass::MimeMismatch,
            "attachment MIME claim does not match its content",
        )
    }

    pub(crate) const fn unsupported_media() -> Self {
        Self::new(
            AttachmentStoreErrorClass::UnsupportedMedia,
            "attachment media format is unsupported",
        )
    }

    pub(crate) const fn invalid_image() -> Self {
        Self::new(
            AttachmentStoreErrorClass::InvalidImage,
            "attachment image metadata is invalid",
        )
    }

    pub(crate) const fn invalid_audio() -> Self {
        Self::new(
            AttachmentStoreErrorClass::InvalidInput,
            "attachment audio metadata is invalid",
        )
    }

    pub(crate) const fn cancelled() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Cancelled,
            "attachment operation was cancelled",
        )
    }

    pub(crate) const fn stopped() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Stopped,
            "attachment service is stopped",
        )
    }

    pub(crate) const fn unavailable() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Unavailable,
            "attachment service is unavailable",
        )
    }

    pub(crate) const fn io() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Io,
            "attachment storage operation failed",
        )
    }

    pub(crate) const fn corrupt() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Corrupt,
            "attachment storage is corrupt",
        )
    }

    pub(crate) const fn session() -> Self {
        Self::new(
            AttachmentStoreErrorClass::Session,
            "attachment metadata could not be committed",
        )
    }

    #[cfg(not(unix))]
    pub(crate) const fn unsupported_security() -> Self {
        Self::new(
            AttachmentStoreErrorClass::UnsupportedSecurity,
            "attachment owner-only security is unsupported on this host",
        )
    }
}
