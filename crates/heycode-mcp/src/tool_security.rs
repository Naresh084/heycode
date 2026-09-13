//! Tool-annotation parsing and user-policy admission.
//!
//! MCP tool annotations are server assertions, never authority. They may help
//! a UI describe a tool but cannot weaken a definition-level allowlist,
//! denylist or approval mode.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{McpApprovalMode, McpChannelError, McpServerId, McpToolPolicy};

const MAX_ANNOTATION_BYTES: usize = 16 * 1024;
const MAX_EXTENSION_FIELDS: usize = 32;

/// Validated advisory MCP tool annotations.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct McpToolAnnotations {
    read_only_hint: Option<bool>,
    destructive_hint: Option<bool>,
    idempotent_hint: Option<bool>,
    open_world_hint: Option<bool>,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl McpToolAnnotations {
    /// Parse server-controlled tool annotations.
    ///
    /// Unknown members remain bounded advisory data. Known members must be
    /// booleans; `null` is not silently treated as absence.
    ///
    /// # Errors
    /// Non-object, oversized or malformed annotations.
    pub fn parse(value: &serde_json::Value) -> Result<Self, McpChannelError> {
        if serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len()) > MAX_ANNOTATION_BYTES
        {
            return Err(McpChannelError::protocol(
                "tool annotations exceed the 16384-byte bound",
            ));
        }
        let object = value.as_object().ok_or(McpChannelError::protocol(
            "tool annotations must be a JSON object",
        ))?;
        let mut extensions = serde_json::Map::new();
        for (key, value) in object {
            if !matches!(
                key.as_str(),
                "readOnlyHint" | "destructiveHint" | "idempotentHint" | "openWorldHint"
            ) {
                extensions.insert(key.clone(), value.clone());
            }
        }
        if extensions.len() > MAX_EXTENSION_FIELDS {
            return Err(McpChannelError::protocol(
                "tool annotations contain too many extension members",
            ));
        }
        Ok(Self {
            read_only_hint: annotation_bool(object, "readOnlyHint")?,
            destructive_hint: annotation_bool(object, "destructiveHint")?,
            idempotent_hint: annotation_bool(object, "idempotentHint")?,
            open_world_hint: annotation_bool(object, "openWorldHint")?,
            extensions,
        })
    }

    /// Server assertion that the tool is read-only.
    #[must_use]
    pub const fn read_only_hint(&self) -> Option<bool> {
        self.read_only_hint
    }

    /// Server assertion that the tool may be destructive.
    #[must_use]
    pub const fn destructive_hint(&self) -> Option<bool> {
        self.destructive_hint
    }

    /// Server assertion that repeated calls are idempotent.
    #[must_use]
    pub const fn idempotent_hint(&self) -> Option<bool> {
        self.idempotent_hint
    }

    /// Server assertion that the tool interacts with an open world.
    #[must_use]
    pub const fn open_world_hint(&self) -> Option<bool> {
        self.open_world_hint
    }

    /// Unknown bounded annotation members.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }
}

impl std::fmt::Debug for McpToolAnnotations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolAnnotations")
            .field("has_read_only_hint", &self.read_only_hint.is_some())
            .field("has_destructive_hint", &self.destructive_hint.is_some())
            .field("has_idempotent_hint", &self.idempotent_hint.is_some())
            .field("has_open_world_hint", &self.open_world_hint.is_some())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

fn annotation_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<bool>, McpChannelError> {
    match object.get(field) {
        None => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(McpChannelError::protocol(
            "known tool annotations must be booleans when present",
        )),
    }
}

/// Effective user-policy result for one MCP tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpToolAdmission {
    /// Tool is outside an exact allowlist and must not be published.
    Hidden,
    /// Tool is explicitly denied.
    Deny,
    /// Tool requires action-time approval.
    Prompt,
    /// Tool may proceed if every higher-level guard also permits it.
    Allow,
}

/// Action-time host decision for a tool whose exact MCP policy is `Prompt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolApprovalDecision {
    /// Permit the call if all higher-level guards also permitted it.
    Allow,
    /// Refuse before any server request.
    Deny,
}

/// Exact action-time approval request.
///
/// Server annotations are deliberately absent. They are advisory display
/// facts and structurally cannot influence this host decision.
#[derive(Clone)]
pub struct McpToolApprovalRequest {
    server: McpServerId,
    tool: String,
    qualified_tool: String,
    arguments: serde_json::Value,
}

impl McpToolApprovalRequest {
    /// Construct one exact product approval request.
    ///
    /// Server annotations are not accepted by this constructor and therefore
    /// cannot enter an adapter that uses it.
    #[must_use]
    pub fn new(
        server: McpServerId,
        tool: String,
        qualified_tool: String,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            server,
            tool,
            qualified_tool,
            arguments,
        }
    }

    /// Server whose definition owns the policy.
    #[must_use]
    pub const fn server(&self) -> &McpServerId {
        &self.server
    }

    /// Unqualified remote tool name.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// Public model-facing tool name.
    #[must_use]
    pub fn qualified_tool(&self) -> &str {
        &self.qualified_tool
    }

    /// Arguments for the human approval surface.
    #[must_use]
    pub const fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
}

impl std::fmt::Debug for McpToolApprovalRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolApprovalRequest")
            .field("server", &self.server)
            .field("tool", &self.tool)
            .field("qualified_tool", &self.qualified_tool)
            .field(
                "argument_bytes",
                &serde_json::to_vec(&self.arguments).map_or(usize::MAX, |bytes| bytes.len()),
            )
            .finish()
    }
}

/// Product-owned action-time approval broker for MCP13.
#[async_trait]
pub trait McpToolApprovalHandler: Send + Sync {
    /// Decide one exact server/tool call.
    async fn decide(
        &self,
        request: McpToolApprovalRequest,
        cancellation: CancellationToken,
    ) -> McpToolApprovalDecision;
}

/// Resolve user policy after parsing server annotations.
///
/// `annotations` is deliberately not consulted for authority: even a server
/// claiming `readOnlyHint=true` cannot override Prompt or Deny.
#[must_use]
pub fn resolve_mcp_tool_admission(
    policy: &McpToolPolicy,
    tool: &str,
    annotations: &McpToolAnnotations,
) -> McpToolAdmission {
    let _advisory_facts = (
        annotations.read_only_hint(),
        annotations.destructive_hint(),
        annotations.idempotent_hint(),
        annotations.open_world_hint(),
    );
    if policy
        .enabled_tools()
        .is_some_and(|enabled| !enabled.contains(tool))
    {
        return McpToolAdmission::Hidden;
    }
    if policy.disabled_tools().contains(tool) {
        return McpToolAdmission::Deny;
    }
    match policy
        .per_tool_approval()
        .get(tool)
        .copied()
        .unwrap_or_else(|| policy.default_approval())
    {
        McpApprovalMode::Prompt => McpToolAdmission::Prompt,
        McpApprovalMode::Allow => McpToolAdmission::Allow,
        McpApprovalMode::Deny => McpToolAdmission::Deny,
    }
}
