//! Operation-time credential-reference bindings for MCP transports.

use std::collections::BTreeMap;

use heycode_credentials::{CredentialQuery, CredentialsService};

use crate::McpSecretReference;

const MAX_SECRET_BYTES: usize = 16 * 1024;

/// How one resolved credential is materialized at its transport boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCredentialEncoding {
    /// Exact secret bytes, for stdio environment/argv or non-Bearer headers.
    Raw,
    /// `Bearer ` followed by the exact secret.
    Bearer,
}

/// Safe binding from an MCP reference to the credential registry's exact
/// reference and semantic kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCredentialBinding {
    query: CredentialQuery,
    encoding: McpCredentialEncoding,
}

impl McpCredentialBinding {
    /// Build one safe operation-time binding.
    #[must_use]
    pub const fn new(query: CredentialQuery, encoding: McpCredentialEncoding) -> Self {
        Self { query, encoding }
    }

    /// Credential registry query; contains no value.
    #[must_use]
    pub const fn query(&self) -> &CredentialQuery {
        &self.query
    }

    /// Transport materialization rule.
    #[must_use]
    pub const fn encoding(&self) -> McpCredentialEncoding {
        self.encoding
    }
}

/// Complete reference binding set for one or more exact MCP definitions.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct McpCredentialBindings {
    rows: BTreeMap<McpSecretReference, McpCredentialBinding>,
}

impl McpCredentialBindings {
    /// Empty set for literal/no-auth definitions.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
        }
    }

    /// Insert one exact reference binding.
    ///
    /// # Errors
    /// Duplicate references and a registry query naming a different reference
    /// fail before a connection can resolve anything.
    pub fn insert(
        &mut self,
        reference: McpSecretReference,
        binding: McpCredentialBinding,
    ) -> Result<(), McpCredentialError> {
        if reference.as_str() != binding.query.reference.as_str() {
            return Err(McpCredentialError::ReferenceMismatch);
        }
        if self.rows.insert(reference, binding).is_some() {
            return Err(McpCredentialError::DuplicateBinding);
        }
        Ok(())
    }

    /// Number of safe reference rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are no credential references.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether this exact safe reference is bound.
    #[must_use]
    pub fn contains(&self, reference: &McpSecretReference) -> bool {
        self.rows.contains_key(reference)
    }

    /// Bound references in deterministic order.
    pub fn references(&self) -> impl Iterator<Item = &McpSecretReference> {
        self.rows.keys()
    }

    /// Safe query/encoding bound to one exact reference.
    #[must_use]
    pub fn binding(&self, reference: &McpSecretReference) -> Option<&McpCredentialBinding> {
        self.rows.get(reference)
    }

    pub(crate) fn materialize(
        &self,
        credentials: &CredentialsService,
        reference: &McpSecretReference,
    ) -> Result<String, McpCredentialError> {
        let binding = self
            .rows
            .get(reference)
            .ok_or(McpCredentialError::MissingBinding)?;
        let secret = credentials
            .resolve(&binding.query)
            .map_err(|_| McpCredentialError::Resolution)?
            .ok_or(McpCredentialError::Unavailable)?;
        let exposed = secret.expose();
        if exposed.is_empty()
            || exposed.len() > MAX_SECRET_BYTES
            || exposed
                .bytes()
                .any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
        {
            return Err(McpCredentialError::InvalidValue);
        }
        Ok(match binding.encoding {
            McpCredentialEncoding::Raw => exposed.to_owned(),
            McpCredentialEncoding::Bearer => format!("Bearer {exposed}"),
        })
    }
}

impl std::fmt::Debug for McpCredentialBindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpCredentialBindings")
            .field("rows", &self.rows)
            .finish()
    }
}

/// Closed, value-free credential binding/materialization failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpCredentialError {
    /// The safe MCP reference and registry query name different slots.
    #[error("MCP credential binding references do not match")]
    ReferenceMismatch,
    /// A reference was bound more than once.
    #[error("MCP credential reference has duplicate bindings")]
    DuplicateBinding,
    /// An exact definition reference has no binding.
    #[error("MCP credential reference has no operation binding")]
    MissingBinding,
    /// Credential registry/provider failed.
    #[error("MCP credential resolution failed")]
    Resolution,
    /// No provider has the exact reference.
    #[error("MCP credential is unavailable")]
    Unavailable,
    /// The resolved bytes cannot safely enter a header/argv/environment slot.
    #[error("MCP credential value is invalid for the transport")]
    InvalidValue,
}
