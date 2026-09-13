//! Durable O09 hook-contribution vocabulary.

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 bytes one hook may contribute to later model context.
pub const MAX_HOOK_CONTRIBUTION_BYTES: usize = 64 * 1024;

const MAX_HOOK_OWNER_BYTES: usize = 128;

/// Hook lifecycle phase retained beside a contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookContributionPhase {
    /// Produced before the surrounded operation.
    Pre,
    /// Produced after the surrounded operation committed.
    Post,
}

/// Lifecycle point that produced a durable hook contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookContributionEvent {
    /// Model-callable tool execution.
    ToolUse,
    /// Agent turn lifecycle.
    Turn,
    /// Session lifecycle.
    Session,
    /// User prompt admission.
    UserPrompt,
    /// Subagent delegation lifecycle.
    Subagent,
    /// MCP server/tool lifecycle.
    McpServer,
}

/// Handler family that produced a durable hook contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookContributionHandler {
    /// Host command handler.
    Command,
    /// Structured model prompt handler.
    Prompt,
    /// Structured subagent handler.
    Subagent,
    /// MCP tool handler.
    McpTool,
}

/// Validation failure for one durable hook contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hook contribution is invalid")]
pub struct HookContributionError;

/// Bounded text and exact value-free provenance committed by an O09 bridge.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookContributionRecord {
    owner: String,
    phase: HookContributionPhase,
    event: HookContributionEvent,
    handler: HookContributionHandler,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boundary: Option<heycode_core::UntrustedContentBoundary>,
    text: String,
}

impl std::fmt::Debug for HookContributionRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookContributionRecord")
            .field("owner", &self.owner)
            .field("phase", &self.phase)
            .field("event", &self.event)
            .field("handler", &self.handler)
            .field("boundary", &self.boundary)
            .field("text_len", &self.text.len())
            .finish()
    }
}

impl HookContributionRecord {
    /// Validate one complete durable contribution.
    ///
    /// # Errors
    /// Owner or text is empty, oversized, or contains unsafe owner controls.
    pub fn new(
        owner: impl Into<String>,
        phase: HookContributionPhase,
        event: HookContributionEvent,
        handler: HookContributionHandler,
        boundary: Option<heycode_core::UntrustedContentBoundary>,
        text: impl Into<String>,
    ) -> Result<Self, HookContributionError> {
        let record = Self {
            owner: owner.into(),
            phase,
            event,
            handler,
            boundary,
            text: text.into(),
        };
        record.validate()?;
        Ok(record)
    }

    /// Hook/plugin owner attribution.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> HookContributionPhase {
        self.phase
    }

    /// Lifecycle event.
    #[must_use]
    pub const fn event(&self) -> HookContributionEvent {
        self.event
    }

    /// Handler family.
    #[must_use]
    pub const fn handler(&self) -> HookContributionHandler {
        self.handler
    }

    /// External-data boundary, when inherited or intrinsic.
    #[must_use]
    pub const fn boundary(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        self.boundary
    }

    /// Exact bounded contribution text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Model projection used by both neutral and exact-route folds.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        self.boundary.map_or_else(
            || self.text.clone(),
            |boundary| boundary.render_for_model(&self.text),
        )
    }

    pub(crate) fn validate(&self) -> Result<(), HookContributionError> {
        let owner = self.owner.as_bytes();
        if !(1..=MAX_HOOK_OWNER_BYTES).contains(&owner.len())
            || self.owner.trim() != self.owner
            || owner
                .iter()
                .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
            || self.text.is_empty()
            || self.text.len() > MAX_HOOK_CONTRIBUTION_BYTES
        {
            return Err(HookContributionError);
        }
        Ok(())
    }
}
