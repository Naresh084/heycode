//! Shared settings identity and layer vocabulary.

use crate::SettingsError;

/// Opaque id for one plugin-owned settings section.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingsNamespace(String);

impl SettingsNamespace {
    /// Validate and construct a lowercase kebab-case namespace.
    ///
    /// # Errors
    /// [`SettingsError::InvalidNamespace`] when the value is empty, starts
    /// outside `a-z`, or contains bytes outside `a-z`, `0-9`, and `-`.
    pub fn new(value: impl Into<String>) -> Result<Self, SettingsError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');
        if !valid {
            return Err(SettingsError::InvalidNamespace { value });
        }
        Ok(Self(value))
    }

    /// Stable string representation used at document/UI boundaries.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SettingsNamespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One settings precedence layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsLayer {
    /// Defaults declared by the namespace schema.
    SchemaDefaults,
    /// Composition/deployment values declared by the namespace owner.
    Base,
    /// User-wide persisted values.
    User,
    /// Trusted project persisted values.
    Project,
    /// Ephemeral values from this process's command line or session controls.
    /// They beat every persisted layer but never a managed lock, and an
    /// in-session user write drops them for that namespace.
    Override,
    /// Administrator-managed final constraint, highest in resolution.
    Managed,
}

impl std::fmt::Display for SettingsLayer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SchemaDefaults => "schema defaults",
            Self::Base => "base",
            Self::User => "user",
            Self::Project => "project",
            Self::Override => "process override",
            Self::Managed => "managed",
        })
    }
}

/// When a namespace owner can apply a changed setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SettingsApplies {
    /// The owner can apply it without process restart.
    #[default]
    Live,
    /// A process restart is required.
    Restart,
}

/// Origin of a committed settings snapshot transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsUpdateSource {
    /// In-process user-section replacement.
    UserWrite,
    /// In-process override replacement; persisted documents are unchanged.
    OverrideWrite,
    /// Provider published externally reloaded documents.
    ProviderReload,
}
