//! Immutable local session creation metadata and fork lineage.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_CWD_BYTES: usize = 4096;
const MAX_RUNTIME_ID_BYTES: usize = 128;

/// Stable origin of one locally created session stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    /// Interactive terminal composer.
    Interactive,
    /// Non-interactive command invocation.
    Headless,
    /// Agent Client Protocol caller.
    Acp,
    /// Child agent orchestration.
    Subagent,
    /// Durable scheduled work.
    Scheduled,
    /// Provider-owned delegated coding-agent runtime.
    Delegated,
    /// A child created from a verified session prefix.
    Fork,
}

/// Invalid immutable session metadata or lineage.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionMetadataError {
    /// The working directory is not a safe absolute local path.
    #[error("invalid session creation cwd")]
    InvalidCwd,
    /// The runtime id is not bounded lowercase kebab-case.
    #[error("invalid session creation runtime id")]
    InvalidRuntime,
    /// Fork source and parent lineage disagree.
    #[error("invalid session creation source/lineage")]
    InvalidLineage,
    /// Parent identity cannot be used as one sessions-root component.
    #[error("invalid parent session id")]
    InvalidParentId,
    /// Prefix proof is not lowercase SHA-256.
    #[error("invalid parent prefix digest")]
    InvalidPrefixDigest,
}

/// Safe immutable cwd/runtime/source facts for one local session stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreationMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    source: SessionSource,
}

impl SessionCreationMetadata {
    /// Construct validated immutable creation facts.
    ///
    /// # Errors
    /// Cwd is not absolute/canonical-shaped UTF-8 or runtime is not bounded
    /// lowercase kebab-case.
    pub fn new(
        cwd: Option<PathBuf>,
        runtime: Option<String>,
        source: SessionSource,
    ) -> Result<Self, SessionMetadataError> {
        let metadata = Self {
            cwd,
            runtime,
            source,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Creation-time working directory, when known.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Runtime id selected for this local stream, when known.
    #[must_use]
    pub fn runtime(&self) -> Option<&str> {
        self.runtime.as_deref()
    }

    /// Origin of this local stream.
    #[must_use]
    pub const fn source(&self) -> SessionSource {
        self.source
    }

    pub(crate) fn inherited_for_fork(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            runtime: self.runtime.clone(),
            source: SessionSource::Fork,
        }
    }

    fn validate(&self) -> Result<(), SessionMetadataError> {
        if self.cwd.as_deref().is_some_and(|cwd| !valid_cwd(cwd)) {
            return Err(SessionMetadataError::InvalidCwd);
        }
        if self
            .runtime
            .as_deref()
            .is_some_and(|runtime| !valid_runtime_id(runtime))
        {
            return Err(SessionMetadataError::InvalidRuntime);
        }
        Ok(())
    }
}

/// Verified shared-prefix relationship to one parent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionParent {
    parent_session_id: heycode_core::SessionId,
    seed_event_count: u64,
    prefix_sha256: String,
}

impl SessionParent {
    pub(crate) fn new(
        parent_session_id: heycode_core::SessionId,
        seed_event_count: u64,
        prefix_sha256: String,
    ) -> Result<Self, SessionMetadataError> {
        let parent = Self {
            parent_session_id,
            seed_event_count,
            prefix_sha256,
        };
        parent.validate()?;
        Ok(parent)
    }

    /// Parent session whose prefix is inherited.
    #[must_use]
    pub const fn parent_session_id(&self) -> &heycode_core::SessionId {
        &self.parent_session_id
    }

    /// Number of inherited events, equivalent to boundary seq + 1.
    #[must_use]
    pub const fn seed_event_count(&self) -> u64 {
        self.seed_event_count
    }

    pub(crate) fn prefix_sha256(&self) -> &str {
        &self.prefix_sha256
    }

    fn validate(&self) -> Result<(), SessionMetadataError> {
        if !valid_session_component(self.parent_session_id.as_str()) {
            return Err(SessionMetadataError::InvalidParentId);
        }
        if !valid_sha256(&self.prefix_sha256) {
            return Err(SessionMetadataError::InvalidPrefixDigest);
        }
        Ok(())
    }
}

/// One physical stream's immutable creation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreation {
    metadata: SessionCreationMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<SessionParent>,
}

impl SessionCreation {
    /// Construct one non-fork creation record.
    ///
    /// # Errors
    /// Metadata claims fork origin without parent lineage.
    pub fn new(metadata: SessionCreationMetadata) -> Result<Self, SessionMetadataError> {
        let creation = Self {
            metadata,
            parent: None,
        };
        creation.validate()?;
        Ok(creation)
    }

    pub(crate) fn fork(
        metadata: SessionCreationMetadata,
        parent: SessionParent,
    ) -> Result<Self, SessionMetadataError> {
        let creation = Self {
            metadata,
            parent: Some(parent),
        };
        creation.validate()?;
        Ok(creation)
    }

    /// Safe immutable cwd/runtime/source facts.
    #[must_use]
    pub const fn metadata(&self) -> &SessionCreationMetadata {
        &self.metadata
    }

    /// Shared-prefix parent, only for a fork.
    #[must_use]
    pub const fn parent(&self) -> Option<&SessionParent> {
        self.parent.as_ref()
    }

    pub(crate) fn validate(&self) -> Result<(), SessionMetadataError> {
        self.metadata.validate()?;
        if let Some(parent) = &self.parent {
            parent.validate()?;
        }
        if (self.metadata.source == SessionSource::Fork) != self.parent.is_some() {
            return Err(SessionMetadataError::InvalidLineage);
        }
        Ok(())
    }
}

pub(crate) fn valid_session_component(value: &str) -> bool {
    let first = value.bytes().next();
    let last = value.bytes().next_back();
    !value.is_empty()
        && value.len() <= 128
        && first.is_some_and(|byte| byte.is_ascii_alphanumeric())
        && last.is_some_and(|byte| byte.is_ascii_alphanumeric())
        && !windows_reserved_name(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn valid_runtime_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RUNTIME_ID_BYTES
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

pub(crate) fn valid_cwd(cwd: &Path) -> bool {
    let Some(text) = cwd.to_str() else {
        return false;
    };
    let normalized = cwd.components().collect::<PathBuf>();
    cwd.is_absolute()
        && text.len() <= MAX_CWD_BYTES
        && !text.chars().any(unsafe_metadata_char)
        && normalized.as_os_str() == cwd.as_os_str()
        && !cwd
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn unsafe_metadata_char(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

fn windows_reserved_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                suffix.len() == 1
                    && suffix
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
            })
}
