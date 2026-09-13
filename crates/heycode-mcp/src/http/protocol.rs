//! Closed protocol revisions and the credential-grade session identifier.

use std::fmt::{Debug, Formatter};

use super::McpHttpError;

const MAX_SESSION_ID_BYTES: usize = 512;

/// Streamable HTTP protocol revisions heycode speaks.
///
/// These are the **legacy** revisions — the ones that establish a session with
/// an `initialize` handshake. heycode targets them because that is what deployed
/// servers speak today and because MCP07/MCP08 assume session ids.
///
/// The set is closed on purpose: an `initialize` result naming any other
/// revision disconnects instead of guessing a compatible dialect. Revision
/// `2026-07-28` is **current** and removed sessions, the GET stream and
/// resumability outright — a different transport contract, not a newer dialect
/// of this one. Supporting it later means adding an arm here and the stateless
/// request shape behind it, not rewriting this engine.
/// <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http>
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpProtocolVersion {
    /// `2025-11-25` — latest revision with the `initialize` handshake.
    V20251125,
    /// `2025-06-18` — first revision defining `MCP-Protocol-Version`.
    V20250618,
}

impl McpProtocolVersion {
    /// Revision offered in the `InitializeRequest`.
    pub const LATEST: Self = Self::V20251125;

    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V20251125 => "2025-11-25",
            Self::V20250618 => "2025-06-18",
        }
    }

    /// Resolve a server-negotiated revision, or `None` when unsupported.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "2025-11-25" => Some(Self::V20251125),
            "2025-06-18" => Some(Self::V20250618),
            _ => None,
        }
    }
}

/// Server-assigned Streamable HTTP session identifier.
///
/// The value authorizes every subsequent request on the session, so it is
/// treated as credential material: `Debug` is redacted, there is no `Display`
/// or serialization, and exposure is an explicit operation.
#[derive(Clone, PartialEq, Eq)]
pub struct McpSessionId(String);

impl McpSessionId {
    /// Validate a session identifier taken from a response header.
    ///
    /// # Errors
    /// Identifiers must be 1..=512 visible ASCII bytes (`0x21`..=`0x7E`), which
    /// also prevents whitespace or control bytes from re-entering a request
    /// header.
    pub fn new(value: impl Into<String>) -> Result<Self, McpHttpError> {
        let value = value.into();
        let valid = (1..=MAX_SESSION_ID_BYTES).contains(&value.len())
            && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte));
        if !valid {
            return Err(McpHttpError::invalid(
                "session id",
                "1..=512 visible ASCII bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Explicitly expose the identifier to a request builder.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Debug for McpSessionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpSessionId([REDACTED])")
    }
}
