//! Stable, redacted registry failures.

/// Definition, registration and generation-publication failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpRegistryError {
    /// A boundary field violated its closed contract.
    #[error("invalid MCP {field}: {requirement}")]
    InvalidField {
        /// Stable field label; never the rejected value.
        field: &'static str,
        /// Static requirement text.
        requirement: &'static str,
    },
    /// Two definition owners claimed the same stable server id.
    #[error("MCP server `{id}` is already defined")]
    DuplicateServer {
        /// Contested validated id.
        id: String,
    },
    /// A requested definition is absent.
    #[error("MCP server `{id}` is not defined")]
    ServerNotFound {
        /// Missing validated id.
        id: String,
    },
    /// A disabled definition cannot acquire a live transport.
    #[error("MCP server `{id}` is disabled")]
    ServerDisabled {
        /// Disabled validated id.
        id: String,
    },
    /// Two connection providers attempted to own one definition.
    #[error("MCP server `{id}` already has a connection provider")]
    DuplicateConnection {
        /// Contested validated id.
        id: String,
    },
    /// A disposed/replaced owner attempted to publish state.
    #[error("MCP server `{id}` registration is no longer active")]
    StaleRegistration {
        /// Validated id whose registration changed.
        id: String,
    },
    /// Shared state was poisoned.
    #[error("MCP registry is unavailable")]
    RegistryUnavailable,
    /// Owning registry plugin has completed shutdown.
    #[error("MCP registry is closed")]
    RegistryClosed,
    /// The global inspectable revision reached its numeric limit.
    #[error("MCP registry revision is exhausted")]
    RevisionExhausted,
    /// One server's successful connection generation counter was exhausted.
    #[error("MCP server `{id}` generation counter is exhausted")]
    GenerationExhausted {
        /// Validated server id.
        id: String,
    },
}

impl McpRegistryError {
    pub(super) const fn invalid(field: &'static str, requirement: &'static str) -> Self {
        Self::InvalidField { field, requirement }
    }
}
