//! Stable, body-free Streamable HTTP failures.

use crate::channel::McpChannelError;

/// Streamable HTTP transport and protocol failures.
///
/// No endpoint URL, header value, session identifier or response body byte may
/// enter this type; only a status code, a JSON-RPC code and static requirement
/// text are retained.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpHttpError {
    /// A boundary field violated its closed contract.
    #[error("invalid MCP HTTP {field}: {requirement}")]
    InvalidField {
        /// Stable field label; never the rejected value.
        field: &'static str,
        /// Static requirement text.
        requirement: &'static str,
    },
    /// DNS/connect/TLS/body failure below the protocol.
    #[error("MCP HTTP transport failed")]
    Transport,
    /// An explicit operation budget expired.
    #[error("MCP HTTP request timed out")]
    TimedOut,
    /// Caller cancellation won before the next commit point.
    #[error("MCP HTTP request was cancelled")]
    Cancelled,
    /// The server answered with an unusable status.
    #[error("MCP server answered HTTP {status}")]
    Status {
        /// Response status code.
        status: u16,
    },
    /// The server answered with a JSON-RPC error object.
    #[error("MCP request failed with JSON-RPC code {code}")]
    Rpc {
        /// JSON-RPC error code; never the server message.
        code: i64,
    },
    /// The server violated a closed transport contract.
    #[error("MCP protocol contract failed: {requirement}")]
    Protocol {
        /// Static requirement text.
        requirement: &'static str,
    },
    /// Authorization is absent or rejected.
    #[error("MCP server requires authorization")]
    Unauthorized,
}

impl McpHttpError {
    pub(crate) const fn protocol(requirement: &'static str) -> Self {
        Self::Protocol { requirement }
    }

    pub(crate) const fn invalid(field: &'static str, requirement: &'static str) -> Self {
        Self::InvalidField { field, requirement }
    }
}

impl From<McpHttpError> for McpChannelError {
    fn from(error: McpHttpError) -> Self {
        match error {
            McpHttpError::InvalidField { .. } => {
                Self::protocol("MCP HTTP request field is invalid")
            }
            McpHttpError::Transport | McpHttpError::Status { .. } => Self::Transport,
            McpHttpError::TimedOut => Self::TimedOut,
            McpHttpError::Cancelled => Self::Cancelled,
            McpHttpError::Rpc { code } => Self::Rpc { code },
            McpHttpError::Protocol { requirement } => Self::Protocol { requirement },
            McpHttpError::Unauthorized => Self::Unauthorized,
        }
    }
}

impl From<McpChannelError> for McpHttpError {
    fn from(error: McpChannelError) -> Self {
        match error {
            McpChannelError::Transport => Self::Transport,
            McpChannelError::TimedOut => Self::TimedOut,
            McpChannelError::Cancelled => Self::Cancelled,
            McpChannelError::Rpc { code } => Self::Rpc { code },
            McpChannelError::Protocol { requirement } => Self::Protocol { requirement },
            McpChannelError::Unauthorized => Self::Unauthorized,
            McpChannelError::Conflict => Self::protocol("MCP generation conflicted"),
        }
    }
}
