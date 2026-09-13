//! Validated opaque MCP registry identities.

use std::fmt::{Display, Formatter};

use serde::Serialize;

use super::McpRegistryError;

/// Stable lowercase server namespace used by public MCP contributions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct McpServerId(String);

impl McpServerId {
    /// Validate a server id before registry use.
    ///
    /// # Errors
    /// IDs must be 1..=64 lowercase ASCII alphanumeric, `-`, or `_` bytes,
    /// start with a letter and end with an alphanumeric byte.
    pub fn new(value: impl Into<String>) -> Result<Self, McpRegistryError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=64).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'-' | b'_')
            });
        if !valid {
            return Err(McpRegistryError::invalid(
                "server id",
                "1..=64 lowercase ASCII namespace bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Stable namespace text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for McpServerId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable lowercase kebab-case connection-provider implementation id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct McpConnectionProviderId(String);

impl McpConnectionProviderId {
    /// Validate a connection-provider id.
    ///
    /// # Errors
    /// IDs must be 1..=64 lowercase kebab-case ASCII bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, McpRegistryError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=64).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.contains("--");
        if !valid {
            return Err(McpRegistryError::invalid(
                "connection provider id",
                "1..=64 lowercase kebab-case ASCII bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Stable provider id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for McpConnectionProviderId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Non-secret reference to credential material resolved by a later provider.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct McpSecretReference(String);

impl McpSecretReference {
    /// Validate a safe credential-reference identifier.
    ///
    /// # Errors
    /// References must be 1..=256 trimmed ASCII bytes using letters, digits,
    /// `.`, `_`, `-`, `/`, or `:`.
    pub fn new(value: impl Into<String>) -> Result<Self, McpRegistryError> {
        let value = value.into();
        let valid = (1..=256).contains(&value.len())
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
            });
        if !valid {
            return Err(McpRegistryError::invalid(
                "credential reference",
                "1..=256 safe ASCII identifier bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Safe reference text; never credential material.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for McpSecretReference {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
