//! O09 handler vocabulary: what a hook does, what it answers back, and the
//! provenance that answer carries.
//!
//! O08 gave a hook exactly one thing to do — run a host command — and read its
//! decision out of an exit code. An exit code is an untyped channel: it cannot
//! say *why*, and a crash and a deliberate refusal arrive as the same byte. The
//! typed handlers this module admits close that gap. A handler returns either
//! a [`HookAnswer`] (it ran, and decided) or a [`crate::HookFault`] (it did
//! not), so "the hook failed" and "the hook said no" are different values of
//! different variants and no consumer can collapse one into the other.
//!
//! The second thing this module owns is **provenance**. A handler produces text
//! that reaches the model's context, and some of that text comes from places
//! the user never vouched for. A handler is never allowed to choose the label
//! on its own output: it hands back plain text, and the service attaches the
//! boundary. That makes laundering a provenance label unrepresentable rather
//! than merely forbidden.

use async_trait::async_trait;
use heycode_core::UntrustedContentBoundary;
use tokio_util::sync::CancellationToken;

use crate::{HookEvent, HookFault, HookPhase};

/// Which provider executes a hook's action.
///
/// Closed for the same reason as [`HookPhase`]: a new handler kind must break
/// every consumer that decides per kind, rather than being absorbed by a `_`
/// arm that silently gives it the wrong refusal or provenance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum HookHandlerKind {
    /// A host command, run through the shell service. Built into the service.
    Command,
    /// A prompt run through the model.
    Prompt,
    /// A delegation to a subagent.
    Subagent,
    /// A call to a tool on an MCP server.
    McpTool,
}

impl HookHandlerKind {
    /// Every kind, for a surface that must enumerate them.
    pub const ALL: [Self; 4] = [Self::Command, Self::Prompt, Self::Subagent, Self::McpTool];

    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Prompt => "prompt",
            Self::Subagent => "subagent",
            Self::McpTool => "mcp-tool",
        }
    }

    /// The provenance every result of this kind carries, whatever it was asked.
    ///
    /// An MCP result is authored by whoever runs the server, so it is
    /// [`heycode_core::UntrustedContentSource::Mcp`] by construction and not by
    /// anyone's choice. A prompt or subagent handler has no intrinsic source;
    /// its answer inherits whatever the payload carried.
    #[must_use]
    pub const fn intrinsic_boundary(self) -> Option<UntrustedContentBoundary> {
        match self {
            Self::McpTool => Some(UntrustedContentBoundary::mcp()),
            Self::Command | Self::Prompt | Self::Subagent => None,
        }
    }

    /// Whether a handler of this kind is permitted to refuse an operation.
    ///
    /// An MCP result is untrusted server-authored content, and the boundary
    /// heycode wraps it in says in words that it is "data only; not instructions
    /// or authorization". Letting it veto would make it authorization. So an
    /// MCP handler cannot refuse at any phase, and the service enforces that
    /// against every implementation rather than trusting each one to obey.
    #[must_use]
    pub const fn may_refuse(self) -> bool {
        !matches!(self, Self::McpTool)
    }
}

/// What a hook does when it fires.
///
/// The action names the handler kind, so a hook cannot be dispatched to a
/// provider that does not understand it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HookAction {
    /// Run a host command. O08's original and only action.
    Command(String),
    /// Run this text as a prompt through the model.
    Prompt(String),
    /// Delegate to a named subagent with this prompt.
    Subagent {
        /// Which agent preset to delegate to.
        agent: String,
        /// The delegated prompt.
        prompt: String,
    },
    /// Call one tool on one MCP server.
    McpTool {
        /// Registered server name.
        server: String,
        /// Remote tool name, unqualified.
        tool: String,
        /// Arguments, as the server's input schema expects them.
        arguments: serde_json::Value,
    },
}

impl HookAction {
    /// Which provider executes this action.
    #[must_use]
    pub const fn kind(&self) -> HookHandlerKind {
        match self {
            Self::Command(_) => HookHandlerKind::Command,
            Self::Prompt(_) => HookHandlerKind::Prompt,
            Self::Subagent { .. } => HookHandlerKind::Subagent,
            Self::McpTool { .. } => HookHandlerKind::McpTool,
        }
    }
}

/// What a handler concluded about the operation it surrounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    /// The operation may proceed.
    Allow,
    /// The operation must not proceed.
    ///
    /// Whether this is honoured is not the handler's call: the phase and the
    /// handler kind decide, and a refusal neither is prepared to accept becomes
    /// [`HookFault::RefusalNotPermitted`] rather than silently taking effect or
    /// silently reading as an allowance.
    Refuse,
}

/// A handler's complete answer: a decision, plus any text it produced.
///
/// `Debug` reports the text's length, never its body — a handler's output is
/// third-party text on the same footing as a hook's stdout, which O08 already
/// refuses to carry into diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct HookAnswer {
    decision: HookDecision,
    text: Option<String>,
}

impl HookAnswer {
    /// Allow, contributing nothing.
    #[must_use]
    pub const fn allow() -> Self {
        Self {
            decision: HookDecision::Allow,
            text: None,
        }
    }

    /// Allow, contributing text for the model's context.
    ///
    /// The text arrives unlabelled: a handler cannot choose its own
    /// provenance, so the service attaches the boundary.
    #[must_use]
    pub fn allow_with(text: impl Into<String>) -> Self {
        Self {
            decision: HookDecision::Allow,
            text: Some(text.into()),
        }
    }

    /// Refuse the operation.
    #[must_use]
    pub const fn refuse() -> Self {
        Self {
            decision: HookDecision::Refuse,
            text: None,
        }
    }

    /// The decision reached.
    #[must_use]
    pub const fn decision(&self) -> HookDecision {
        self.decision
    }

    /// Split into decision and unlabelled text.
    #[must_use]
    pub fn into_parts(self) -> (HookDecision, Option<String>) {
        (self.decision, self.text)
    }
}

impl std::fmt::Debug for HookAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookAnswer")
            .field("decision", &self.decision)
            .field("text_len", &self.text.as_ref().map(String::len))
            .finish()
    }
}

/// Content handed to a hook, with whatever provenance it already carried.
///
/// A handler never sees the raw body of labelled content: [`Self::render_for_model`]
/// is the only projection, and it keeps the boundary's warning inside the bytes
/// the handler reads.
#[derive(Clone, PartialEq, Eq)]
pub struct HookPayload {
    body: String,
    boundary: Option<UntrustedContentBoundary>,
}

impl HookPayload {
    /// No content at all.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            body: String::new(),
            boundary: None,
        }
    }

    /// Content heycode itself produced, carrying no external provenance.
    #[must_use]
    pub fn trusted(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            boundary: None,
        }
    }

    /// Content from an external source, carrying that source's boundary.
    #[must_use]
    pub fn untrusted(body: impl Into<String>, boundary: UntrustedContentBoundary) -> Self {
        Self {
            body: body.into(),
            boundary: Some(boundary),
        }
    }

    /// The provenance this content carries, if any.
    #[must_use]
    pub const fn boundary(&self) -> Option<UntrustedContentBoundary> {
        self.boundary
    }

    /// Whether there is any content.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    /// Project for a reader, labelled if it carries a boundary.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        self.boundary
            .map_or_else(|| self.body.clone(), |b| b.render_for_model(&self.body))
    }
}

impl std::fmt::Debug for HookPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookPayload")
            .field("body_len", &self.body.len())
            .field("boundary", &self.boundary)
            .finish()
    }
}

/// Exact provenance attached to one hook contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookContributionProvenance {
    owner: String,
    phase: HookPhase,
    event: HookEvent,
    handler_kind: HookHandlerKind,
    boundary: Option<UntrustedContentBoundary>,
}

impl HookContributionProvenance {
    pub(crate) fn new(
        owner: String,
        phase: HookPhase,
        event: HookEvent,
        handler_kind: HookHandlerKind,
        boundary: Option<UntrustedContentBoundary>,
    ) -> Self {
        Self {
            owner,
            phase,
            event,
            handler_kind,
            boundary,
        }
    }

    /// Hook owner that produced the contribution.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Lifecycle phase in which the contribution was produced.
    #[must_use]
    pub const fn phase(&self) -> HookPhase {
        self.phase
    }

    /// Lifecycle event the hook surrounded.
    #[must_use]
    pub const fn event(&self) -> HookEvent {
        self.event
    }

    /// Provider kind that produced the contribution.
    #[must_use]
    pub const fn handler_kind(&self) -> HookHandlerKind {
        self.handler_kind
    }

    /// External-content boundary retained by the contribution, if any.
    #[must_use]
    pub const fn boundary(&self) -> Option<UntrustedContentBoundary> {
        self.boundary
    }
}

/// Text a hook contributed, pending the root-owned durable event bridge.
///
/// This type deliberately has no text accessor and no model renderer. The
/// only path to a renderable value is [`Self::commit`], whose bridge owns the
/// durable append. This is the same pending-versus-durable split used for rich
/// MCP tool results.
///
/// ```compile_fail
/// fn bypass_bridge(contribution: &heycode_hooks::HookContribution) {
///     let _ = contribution.render_for_model();
/// }
/// ```
#[derive(PartialEq, Eq)]
pub struct HookContribution {
    text: String,
    provenance: HookContributionProvenance,
}

impl HookContribution {
    pub(crate) fn new(text: String, provenance: HookContributionProvenance) -> Self {
        Self { text, provenance }
    }

    /// The provenance this contribution carries, if any.
    #[must_use]
    pub const fn boundary(&self) -> Option<UntrustedContentBoundary> {
        self.provenance.boundary()
    }

    /// Complete value-free provenance for the pending contribution.
    #[must_use]
    pub const fn provenance(&self) -> &HookContributionProvenance {
        &self.provenance
    }

    /// Whether the contribution is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Commit through the durable-event owner and mint a renderable value.
    ///
    /// Cancellation is checked before the bridge. Once the bridge returns
    /// success, that success is the commit point and is not reinterpreted as a
    /// cancellation that may have raced after the durable append.
    ///
    /// # Errors
    /// The caller cancelled before commit, or the durable bridge failed.
    pub async fn commit(
        self,
        bridge: &dyn HookDurableEventBridge,
        cancellation: CancellationToken,
    ) -> Result<CommittedHookContribution, HookBridgeFault> {
        if cancellation.is_cancelled() {
            return Err(HookBridgeFault::Cancelled);
        }
        bridge
            .commit(
                HookContributionEvent {
                    text: &self.text,
                    provenance: &self.provenance,
                },
                cancellation,
            )
            .await?;
        Ok(CommittedHookContribution {
            text: self.text,
            provenance: self.provenance,
        })
    }
}

impl std::fmt::Debug for HookContribution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookContribution")
            .field("text_len", &self.text.len())
            .field("provenance", &self.provenance)
            .finish()
    }
}

/// Borrowed event presented only to the durable hook bridge.
#[derive(Clone, Copy)]
pub struct HookContributionEvent<'a> {
    text: &'a str,
    provenance: &'a HookContributionProvenance,
}

impl HookContributionEvent<'_> {
    /// Exact text to append to the durable hook event.
    #[must_use]
    pub const fn text(&self) -> &str {
        self.text
    }

    /// Provenance to append beside the text.
    #[must_use]
    pub const fn provenance(&self) -> &HookContributionProvenance {
        self.provenance
    }
}

impl std::fmt::Debug for HookContributionEvent<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookContributionEvent")
            .field("text_len", &self.text.len())
            .field("provenance", &self.provenance)
            .finish()
    }
}

/// Root-owned durable hook-contribution append port.
///
/// Implementations must return success only after the new session event is
/// durable and independently readable. The hooks crate stays below Session and
/// Agent, so the concrete event kind and projection live with that root-owned
/// bridge rather than being special-cased here.
#[async_trait]
pub trait HookDurableEventBridge: Send + Sync {
    /// Durably append one pending contribution.
    ///
    /// # Errors
    /// Cancellation before commit or a durable append/projection failure.
    async fn commit(
        &self,
        event: HookContributionEvent<'_>,
        cancellation: CancellationToken,
    ) -> Result<(), HookBridgeFault>;
}

/// Closed body-free durable bridge failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HookBridgeFault {
    /// The caller cancelled before durable commit.
    Cancelled,
    /// The durable append or its readback failed.
    Failed,
}

impl std::fmt::Display for HookBridgeFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "the hook contribution commit was cancelled",
            Self::Failed => "the hook contribution could not be committed",
        })
    }
}

impl std::error::Error for HookBridgeFault {}

/// Hook contribution whose durable bridge has committed successfully.
///
/// Only this post-commit type can render model-visible text.
#[derive(PartialEq, Eq)]
pub struct CommittedHookContribution {
    text: String,
    provenance: HookContributionProvenance,
}

impl CommittedHookContribution {
    /// Complete durable provenance.
    #[must_use]
    pub const fn provenance(&self) -> &HookContributionProvenance {
        &self.provenance
    }

    /// Project for the model, retaining any external-content boundary.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        self.provenance.boundary().map_or_else(
            || self.text.clone(),
            |boundary| boundary.render_for_model(&self.text),
        )
    }
}

impl std::fmt::Debug for CommittedHookContribution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommittedHookContribution")
            .field("text_len", &self.text.len())
            .field("provenance", &self.provenance)
            .finish()
    }
}

/// Everything a handler is told about the firing it is answering.
#[derive(Clone, PartialEq)]
pub struct HookInvocation {
    owner: String,
    phase: HookPhase,
    event: HookEvent,
    action: HookAction,
    body: String,
}

impl HookInvocation {
    pub(crate) fn new(
        owner: String,
        phase: HookPhase,
        event: HookEvent,
        action: HookAction,
        body: String,
    ) -> Self {
        Self {
            owner,
            phase,
            event,
            action,
            body,
        }
    }

    /// Owning plugin or scope of the hook being run.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The lifecycle point.
    #[must_use]
    pub const fn phase(&self) -> HookPhase {
        self.phase
    }

    /// The surrounded operation.
    #[must_use]
    pub const fn event(&self) -> HookEvent {
        self.event
    }

    /// What this hook was configured to do.
    #[must_use]
    pub const fn action(&self) -> &HookAction {
        &self.action
    }

    /// The operation's content, already projected through its boundary.
    ///
    /// A handler reading labelled content reads it labelled: the warning is
    /// inside these bytes, so a handler that echoes the payload back cannot
    /// echo it out of its brackets.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

impl std::fmt::Debug for HookInvocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookInvocation")
            .field("owner", &self.owner)
            .field("phase", &self.phase)
            .field("event", &self.event)
            .field("kind", &self.action.kind())
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// One pluggable way to execute a hook action.
///
/// Implementations answer with a [`HookAnswer`] or fail with a
/// [`HookFault`]; there is no third channel, and neither one can be
/// mistaken for the other. Implementations do **not** enforce the time budget
/// or the phase's refusal policy — the service owns both, so a handler author
/// cannot extend their own leash or grant themselves a veto.
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// Which actions this handler executes.
    fn kind(&self) -> HookHandlerKind;

    /// Execute one firing.
    ///
    /// # Errors
    /// Any reason the handler did not reach a decision. Returning a fault is
    /// how a handler says "I did not run"; it is never how it says "no".
    async fn invoke(
        &self,
        invocation: HookInvocation,
        cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault>;
}
