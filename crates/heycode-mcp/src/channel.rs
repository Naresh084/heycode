//! Transport-neutral MCP request boundary shared by every connection provider.
//!
//! Stdio and Streamable HTTP differ in framing, not in the JSON-RPC contract a
//! tool-generation walk needs. Both publish one [`McpRequestChannel`], so the
//! atomic generation owner never learns which transport it is driving.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::prompts::McpPromptHandshake;
use crate::registry::{
    McpCapabilitySet, McpContributionCounts, McpFailureCode, McpGenerationCandidate,
};
use crate::resources::McpResourceCapability;

/// Stable MCP request failure classes. No server body, URL, header or
/// credential byte may enter this type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpChannelError {
    /// The underlying transport is unusable.
    #[error("MCP transport failed")]
    Transport,
    /// An explicit operation budget expired.
    #[error("MCP request timed out")]
    TimedOut,
    /// Caller cancellation won before the next commit point.
    #[error("MCP request was cancelled")]
    Cancelled,
    /// The server answered with a JSON-RPC error object.
    #[error("MCP request failed with JSON-RPC code {code}")]
    Rpc {
        /// JSON-RPC error code; never the server message.
        code: i64,
    },
    /// The server violated a closed protocol contract.
    #[error("MCP protocol contract failed: {requirement}")]
    Protocol {
        /// Static requirement text.
        requirement: &'static str,
    },
    /// Authorization is absent or rejected.
    #[error("MCP server requires authorization")]
    Unauthorized,
    /// A live generation raced another owner or a contribution identity.
    #[error("MCP generation conflicts with a concurrent owner")]
    Conflict,
}

impl McpChannelError {
    /// Stable registry failure class for this error.
    #[must_use]
    pub const fn failure_code(&self) -> McpFailureCode {
        match self {
            Self::Transport => McpFailureCode::Transport,
            Self::TimedOut => McpFailureCode::TimedOut,
            Self::Cancelled => McpFailureCode::Internal,
            Self::Conflict => McpFailureCode::Conflict,
            Self::Rpc { .. } | Self::Protocol { .. } => McpFailureCode::Protocol,
            Self::Unauthorized => McpFailureCode::Unauthorized,
        }
    }

    pub(crate) const fn protocol(requirement: &'static str) -> Self {
        Self::Protocol { requirement }
    }
}

/// One live MCP connection able to answer JSON-RPC requests.
///
/// Implementations own their own framing and session state; the caller only
/// supplies the operation's single cancellation token.
#[async_trait]
pub trait McpRequestChannel: Send + Sync {
    /// Send one JSON-RPC request and return its `result` value.
    ///
    /// # Errors
    /// Transport, timeout, cancellation, JSON-RPC error responses and protocol
    /// contract violations, all without server text.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError>;
}

/// Counts the sibling listings of one connection committed.
///
/// The tool owner publishes the connection's single generation, so it must be
/// told what the resource and prompt walks found. `None` means the listing was
/// not walked — which is a different fact from `Some(0)`, "walked and empty",
/// and refuses to be published as one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpSiblingContributions {
    resources: Option<u32>,
    prompts: Option<u32>,
}

impl McpSiblingContributions {
    /// No sibling listing was walked.
    ///
    /// Valid only for a server that advertised neither capability; otherwise
    /// [`McpServerHandshake::candidate`] refuses rather than report zero.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            resources: None,
            prompts: None,
        }
    }

    /// Record a completed `resources/list` walk.
    #[must_use]
    pub const fn with_resources(mut self, resources: u32) -> Self {
        self.resources = Some(resources);
        self
    }

    /// Record a completed `prompts/list` walk.
    #[must_use]
    pub const fn with_prompts(mut self, prompts: u32) -> Self {
        self.prompts = Some(prompts);
        self
    }

    /// Resources walked, absent when the listing was not performed.
    #[must_use]
    pub const fn resources(self) -> Option<u32> {
        self.resources
    }

    /// Prompts walked, absent when the listing was not performed.
    #[must_use]
    pub const fn prompts(self) -> Option<u32> {
        self.prompts
    }
}

/// Negotiated server identity and capabilities from one `initialize` result.
///
/// This is the single validated view of that result. Tools, resources and
/// prompts evidence and the server's untrusted `instructions` are all parsed
/// here exactly once, so no Consumer has to keep the raw JSON alive to ask a
/// second question of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerHandshake {
    protocol_version: String,
    server_name: String,
    server_version: String,
    capabilities: McpCapabilitySet,
    resources: McpResourceCapability,
    prompts: McpPromptHandshake,
}

impl McpServerHandshake {
    /// Validate one `initialize` result before any Consumer sees it.
    ///
    /// # Errors
    /// Missing/ill-typed `protocolVersion`, `capabilities` or `serverInfo`, and
    /// any field the registry's generation candidate would reject.
    pub fn from_initialize_result(result: &serde_json::Value) -> Result<Self, McpChannelError> {
        let result = result.as_object().ok_or(McpChannelError::protocol(
            "initialize result must be an object",
        ))?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpChannelError::protocol(
                "initialize result is missing protocolVersion",
            ))?
            .to_owned();
        let capabilities = result
            .get("capabilities")
            .and_then(serde_json::Value::as_object)
            .ok_or(McpChannelError::protocol(
                "initialize result is missing capabilities",
            ))?;
        let server_info = result
            .get("serverInfo")
            .and_then(serde_json::Value::as_object)
            .ok_or(McpChannelError::protocol(
                "initialize result is missing serverInfo",
            ))?;
        let server_name = server_info
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpChannelError::protocol("serverInfo is missing name"))?
            .to_owned();
        let server_version = server_info
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpChannelError::protocol("serverInfo is missing version"))?
            .to_owned();
        let handshake = Self {
            protocol_version,
            server_name,
            server_version,
            capabilities: McpCapabilitySet {
                tools: capability_present(capabilities, "tools")?,
                resources: capability_present(capabilities, "resources")?,
                prompts: capability_present(capabilities, "prompts")?,
                logging: capability_present(capabilities, "logging")?,
                roots: capability_present(capabilities, "roots")?,
                elicitation: capability_present(capabilities, "elicitation")?,
                sampling: capability_present(capabilities, "sampling")?,
            },
            resources: McpResourceCapability::from_initialize_result(&serde_json::Value::Object(
                result.clone(),
            ))?,
            prompts: McpPromptHandshake::from_handshake_result(&serde_json::Value::Object(
                result.clone(),
            ))
            .map_err(prompt_handshake_error)?,
        };
        handshake.validate_identity()?;
        Ok(handshake)
    }

    /// Identity-only validation, separate from [`Self::candidate`].
    ///
    /// `candidate` additionally enforces that an advertised listing was walked,
    /// which cannot hold at handshake time — nothing has been listed yet.
    fn validate_identity(&self) -> Result<(), McpChannelError> {
        McpGenerationCandidate::new(
            self.protocol_version.clone(),
            self.server_name.clone(),
            self.server_version.clone(),
            self.capabilities,
            McpContributionCounts::default(),
            0,
        )
        .map(|_validated| ())
        .map_err(|_| McpChannelError::protocol("initialize metadata is invalid"))
    }

    /// Negotiated MCP protocol version exactly as the server spelled it.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Negotiated server name.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Negotiated server version.
    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    /// Negotiated capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> McpCapabilitySet {
        self.capabilities
    }

    /// Build the connection's single generation candidate.
    ///
    /// One connection publishes one candidate carrying all three contribution
    /// counts. A per-family candidate would have to report zero for the families
    /// it did not walk, and "advertises prompts, has none" is a false statement
    /// when the truth is "heycode never asked" — the same lie in every surface that
    /// renders the count.
    ///
    /// # Errors
    /// Identity fields the registry rejects; counts without the matching
    /// negotiated capability; and an advertised listing that was not walked,
    /// which is refused rather than published as zero.
    pub fn candidate(
        &self,
        tools: u32,
        siblings: McpSiblingContributions,
        observed_at_ms: u64,
    ) -> Result<McpGenerationCandidate, McpChannelError> {
        McpGenerationCandidate::new(
            self.protocol_version.clone(),
            self.server_name.clone(),
            self.server_version.clone(),
            self.capabilities,
            McpContributionCounts {
                tools,
                resources: walked(self.capabilities.resources, siblings.resources)?,
                prompts: walked(self.capabilities.prompts, siblings.prompts)?,
            },
            observed_at_ms,
        )
        .map_err(|_| McpChannelError::protocol("initialize metadata is invalid"))
    }

    /// Resource capability evidence from the same `initialize` result.
    #[must_use]
    pub const fn resources(&self) -> McpResourceCapability {
        self.resources
    }

    /// Prompt capability and server-instruction evidence from the same result.
    #[must_use]
    pub const fn prompts(&self) -> &McpPromptHandshake {
        &self.prompts
    }

    /// The server's untrusted `instructions`, tri-state.
    ///
    /// A shortcut onto [`Self::prompts`], because a Consumer that wants the
    /// server's guidance should not have to know MCP09 parses it.
    #[must_use]
    pub const fn instructions(&self) -> &crate::prompts::McpInstructions {
        self.prompts.instructions()
    }
}

/// Resolve one sibling listing's contribution count.
///
/// A capability the server advertised but heycode did not walk cannot be reported
/// as zero: the registry's own contract calls these counts "complete", and zero
/// against a true capability bit reads as "offers this, has none".
const fn walked(advertised: bool, count: Option<u32>) -> Result<u32, McpChannelError> {
    match (advertised, count) {
        (_, Some(count)) => Ok(count),
        (false, None) => Ok(0),
        (true, None) => Err(McpChannelError::protocol(
            "a generation cannot report an advertised listing that was never walked",
        )),
    }
}

/// MCP09 failures reaching the shared channel boundary.
///
/// Only the arms `from_handshake_result` can produce are distinguished; the
/// caller-side argument classes cannot arise from parsing a handshake.
fn prompt_handshake_error(error: crate::prompts::McpPromptError) -> McpChannelError {
    match error {
        crate::prompts::McpPromptError::Channel(channel) => channel,
        crate::prompts::McpPromptError::Protocol { requirement } => {
            McpChannelError::Protocol { requirement }
        }
        _ => McpChannelError::protocol("initialize prompt capability is invalid"),
    }
}

fn capability_present(
    capabilities: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<bool, McpChannelError> {
    match capabilities.get(name) {
        None => Ok(false),
        Some(value) if value.is_object() => Ok(true),
        Some(_) => Err(McpChannelError::protocol(
            "initialize capability must be an object",
        )),
    }
}
