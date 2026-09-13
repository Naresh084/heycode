//! O01 — the subagent provider and continuation contract.
//!
//! Delegation has two independent axes that earlier code conflated into one
//! boolean:
//!
//! * **Seed** — where the child's context comes from ([`SubagentSeed`]).
//! * **Continuation** — whether the child survives its first turn
//!   ([`SubagentContinuation`]).
//!
//! A provider implements delegation for one execution class. The native
//! provider runs a child [`crate::Agent`] in this process; later providers may
//! delegate to an isolated worktree or to an external coding agent. Every one
//! of them answers the same questions and advertises tri-state evidence for
//! what it can actually do, so an unsupported combination is refused rather
//! than silently downgraded.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_llm::CapabilitySupport;
use heycode_session::InboxDelivery;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::jobs::{JobId, JobOutcome, JobRegistry, JobSettlement};

/// Longest accepted provider or subagent identifier.
const MAX_ID_BYTES: usize = 128;
/// Longest accepted human label.
const MAX_LABEL_BYTES: usize = 256;
/// Longest accepted delegated prompt.
const MAX_PROMPT_BYTES: usize = 1024 * 1024;

/// Why a subagent identifier or request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentMetadataError {
    /// Empty, oversized, or non-kebab-case identifier.
    InvalidId,
    /// Empty, oversized, or control-bearing label.
    InvalidLabel,
    /// Blank or oversized prompt.
    InvalidPrompt,
}

impl std::fmt::Display for SubagentMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "subagent identifier is invalid",
            Self::InvalidLabel => "subagent label is invalid",
            Self::InvalidPrompt => "subagent prompt is invalid",
        })
    }
}

impl std::error::Error for SubagentMetadataError {}

/// Validated delegation-provider identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubagentProviderId(String);

impl SubagentProviderId {
    /// Validate one kebab-case provider id.
    ///
    /// # Errors
    /// Empty, oversized, or non-kebab-case input.
    pub fn new(value: impl Into<String>) -> Result<Self, SubagentMetadataError> {
        let value = value.into();
        if !kebab_case(&value) {
            return Err(SubagentMetadataError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated identity of one declarative subagent preset.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubagentPresetId(String);

impl SubagentPresetId {
    /// Validate one kebab-case preset id.
    ///
    /// # Errors
    /// Empty, oversized, or non-kebab-case input.
    pub fn new(value: impl Into<String>) -> Result<Self, SubagentMetadataError> {
        let value = value.into();
        if !kebab_case(&value) {
            return Err(SubagentMetadataError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubagentPresetId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::fmt::Display for SubagentProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated identity of one delegated child instance.
///
/// This is the value the model quotes back to continue or interrupt a child,
/// so it is an opaque newtype rather than a bare string across boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubagentId(String);

impl SubagentId {
    /// Validate one child identifier.
    ///
    /// # Errors
    /// Empty, oversized, or control/whitespace-bearing input.
    pub fn new(value: impl Into<String>) -> Result<Self, SubagentMetadataError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_ID_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
            || value.chars().any(char::is_whitespace)
        {
            return Err(SubagentMetadataError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubagentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Unforgeable authority for one subagent owner within one registry.
///
/// The owner and depth are safe facts; the private token binds them to the
/// registry that minted the root. Child authorities can only derive from an
/// existing authority, so model input cannot reset depth or claim a sibling's
/// ownership domain.
#[derive(Clone)]
pub struct SubagentAuthority {
    token: Arc<()>,
    owner: SubagentId,
    depth: u32,
    may_retain_children: bool,
}

impl SubagentAuthority {
    fn unbound(depth: u32) -> Self {
        Self {
            token: Arc::new(()),
            owner: SubagentId("unbound".to_owned()),
            depth,
            may_retain_children: false,
        }
    }

    fn bound(token: Arc<()>, owner: SubagentId) -> Self {
        Self {
            token,
            owner,
            depth: 0,
            may_retain_children: true,
        }
    }

    /// Owner whose children this authority may inspect or control.
    #[must_use]
    pub const fn owner(&self) -> &SubagentId {
        &self.owner
    }

    /// Current agent nesting depth.
    #[must_use]
    pub const fn depth(&self) -> u32 {
        self.depth
    }

    /// Whether this owner outlives and may retain continuable children.
    #[must_use]
    pub const fn may_retain_children(&self) -> bool {
        self.may_retain_children
    }

    pub(crate) fn child(&self, owner: SubagentId, may_retain_children: bool) -> Self {
        Self {
            token: self.token.clone(),
            owner,
            depth: self.depth.saturating_add(1),
            may_retain_children,
        }
    }

    fn belongs_to(&self, token: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.token, token)
    }
}

impl std::fmt::Debug for SubagentAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubagentAuthority")
            .field("owner", &self.owner)
            .field("depth", &self.depth)
            .field("may_retain_children", &self.may_retain_children)
            .finish()
    }
}

impl PartialEq for SubagentAuthority {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
            && self.owner == other.owner
            && self.depth == other.depth
            && self.may_retain_children == other.may_retain_children
    }
}

impl Eq for SubagentAuthority {}

/// Where a delegated child's starting context comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentSeed {
    /// A new empty durable session. The child sees only its prompt.
    Fresh,
    /// A durable shared-prefix fork of the parent session, so the child
    /// inherits the parent's visible history.
    ForkParent,
}

/// Whether a delegated child survives its first turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentContinuation {
    /// Exactly one turn; the child is closed when it settles and no follow-up
    /// is possible.
    OneShot,
    /// The child stays live for follow-up messages until it is closed.
    Continuable,
}

/// Tri-state evidence for what one provider can actually do.
///
/// `Unknown` never counts as support: a request needing a capability requires
/// exact [`CapabilitySupport::Supported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentCapabilities {
    /// Seeding a child from a parent fork.
    pub fork: CapabilitySupport,
    /// Keeping a child live for follow-up messages.
    pub continuation: CapabilitySupport,
    /// Cancelling a child's in-flight turn without closing it.
    pub interrupt: CapabilitySupport,
}

/// Safe static description of one delegation provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentProviderDescriptor {
    id: SubagentProviderId,
    display: String,
    capabilities: SubagentCapabilities,
}

impl SubagentProviderDescriptor {
    /// Validate one provider descriptor.
    ///
    /// # Errors
    /// Invalid id or display text.
    pub fn new(
        id: impl Into<String>,
        display: impl Into<String>,
        capabilities: SubagentCapabilities,
    ) -> Result<Self, SubagentMetadataError> {
        let display = display.into();
        if display.is_empty()
            || display.len() > MAX_LABEL_BYTES
            || display.chars().any(char::is_control)
        {
            return Err(SubagentMetadataError::InvalidLabel);
        }
        Ok(Self {
            id: SubagentProviderId::new(id)?,
            display,
            capabilities,
        })
    }

    /// Registry identity.
    #[must_use]
    pub const fn id(&self) -> &SubagentProviderId {
        &self.id
    }

    /// Human-facing name.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Advertised evidence.
    #[must_use]
    pub const fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    /// Whether this provider proves it can serve the request's seed and
    /// continuation choices.
    #[must_use]
    pub fn supports(&self, request: &SubagentRequest) -> bool {
        let seed = match request.seed {
            SubagentSeed::Fresh => true,
            SubagentSeed::ForkParent => self.capabilities.fork == CapabilitySupport::Supported,
        };
        let continuation = match request.continuation {
            SubagentContinuation::OneShot => true,
            SubagentContinuation::Continuable => {
                self.capabilities.continuation == CapabilitySupport::Supported
            }
        };
        seed && continuation
    }
}

/// One validated delegation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentRequest {
    label: String,
    prompt: String,
    /// Starting context for the child.
    pub seed: SubagentSeed,
    /// Whether the child outlives its first turn.
    pub continuation: SubagentContinuation,
    provider: Option<SubagentProviderId>,
    authority: SubagentAuthority,
    pub(crate) task: Option<Arc<crate::task_inventory::TaskRecord>>,
    observer: Option<Arc<crate::task_inventory::TaskObserver>>,
    instructions: String,
    configuration_key: Option<SubagentPresetId>,
    config: crate::SubagentConfig,
}

impl SubagentRequest {
    /// Validate one delegation request.
    ///
    /// # Errors
    /// Invalid label or blank/oversized prompt.
    pub fn new(
        label: impl Into<String>,
        prompt: impl Into<String>,
        seed: SubagentSeed,
        continuation: SubagentContinuation,
        depth: u32,
    ) -> Result<Self, SubagentMetadataError> {
        let label = label.into();
        if label.is_empty() || label.len() > MAX_LABEL_BYTES || label.chars().any(char::is_control)
        {
            return Err(SubagentMetadataError::InvalidLabel);
        }
        let prompt = prompt.into();
        if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(SubagentMetadataError::InvalidPrompt);
        }
        Ok(Self {
            label,
            prompt,
            seed,
            continuation,
            provider: None,
            authority: SubagentAuthority::unbound(depth),
            task: None,
            observer: None,
            instructions: String::new(),
            configuration_key: None,
            config: crate::SubagentConfig::default(),
        })
    }

    /// Build a registry-authorized delegation request.
    ///
    /// Model-facing callers receive the authority from their host; neither
    /// owner nor depth is part of the tool schema.
    ///
    /// # Errors
    /// Invalid label or blank/oversized prompt.
    pub fn with_authority(
        label: impl Into<String>,
        prompt: impl Into<String>,
        seed: SubagentSeed,
        continuation: SubagentContinuation,
        authority: SubagentAuthority,
    ) -> Result<Self, SubagentMetadataError> {
        let mut request = Self::new(label, prompt, seed, continuation, authority.depth())?;
        request.authority = authority;
        Ok(request)
    }

    pub(crate) fn with_task_observer(
        mut self,
        observer: Arc<crate::task_inventory::TaskObserver>,
    ) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Select native workspace isolation without changing the prompt or continuation mode.
    #[must_use]
    pub fn with_isolation(mut self, isolation: crate::ChildIsolation) -> Self {
        self.config.isolation = isolation;
        self
    }

    /// Attach validated instruction and configuration layers without changing user input.
    ///
    /// # Errors
    /// Invalid policy or oversized instructions.
    pub fn with_configuration(
        mut self,
        instructions: impl Into<String>,
        config: crate::SubagentConfig,
    ) -> Result<Self, SubagentError> {
        config
            .validate()
            .map_err(|reason| SubagentError::new(SubagentErrorCode::Refused, reason))?;
        let instructions = instructions.into();
        if instructions.len() > MAX_PROMPT_BYTES {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "instructions exceed size limit",
            ));
        }
        self.instructions = instructions;
        self.config = config;
        Ok(self)
    }

    /// Snapshot instructions, policy, provider restriction and stable memory
    /// identity. Seed and continuation are already resolved by the caller.
    /// Aliases preserve source identity, so spelling an alias cannot fork memory.
    ///
    /// # Errors
    /// Invalid execution policy.
    pub fn with_preset(mut self, preset: &SubagentPreset) -> Result<Self, SubagentError> {
        self = self.with_configuration(preset.instructions(), preset.config().clone())?;
        if let Some(provider) = preset.provider() {
            if self
                .provider
                .as_ref()
                .is_some_and(|selected| selected != provider)
            {
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "preset conflicts with requested provider",
                ));
            }
            self.provider = Some(provider.clone());
        }
        self.configuration_key = Some(preset.configuration_key.clone());
        Ok(self)
    }

    /// Stable originating preset identity for persistent memory.
    #[must_use]
    pub fn configuration_key(&self) -> Option<&SubagentPresetId> {
        self.configuration_key.as_ref()
    }

    /// Preset instructions, separate from the exact user task.
    #[must_use]
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Immutable child execution policy.
    #[must_use]
    pub fn config(&self) -> &crate::SubagentConfig {
        &self.config
    }

    /// Human label recorded at delegation time.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Exact delegated prompt.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Registry-bound owner/depth authority.
    #[must_use]
    pub const fn authority(&self) -> &SubagentAuthority {
        &self.authority
    }

    /// Restrict this request to one exact registered provider.
    #[must_use]
    pub fn with_provider(mut self, provider: SubagentProviderId) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Exact requested provider, when a preset selected one.
    #[must_use]
    pub const fn provider(&self) -> Option<&SubagentProviderId> {
        self.provider.as_ref()
    }
}

/// One declarative agent preset resolved before delegation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentPreset {
    id: SubagentPresetId,
    configuration_key: SubagentPresetId,
    display: String,
    description: Option<String>,
    instructions: String,
    provider: Option<SubagentProviderId>,
    seed: SubagentSeed,
    continuation: SubagentContinuation,
    config: crate::SubagentConfig,
}

impl SubagentPreset {
    /// Validate one preset.
    ///
    /// # Errors
    /// Invalid id/display or blank/oversized instructions are rejected.
    pub fn new(
        id: impl Into<String>,
        display: impl Into<String>,
        instructions: impl Into<String>,
        provider: Option<SubagentProviderId>,
        seed: SubagentSeed,
        continuation: SubagentContinuation,
    ) -> Result<Self, SubagentMetadataError> {
        let display = display.into();
        if display.is_empty()
            || display.len() > MAX_LABEL_BYTES
            || display.trim() != display
            || display.chars().any(char::is_control)
        {
            return Err(SubagentMetadataError::InvalidLabel);
        }
        let instructions = instructions.into();
        if instructions.trim().is_empty() || instructions.len() > MAX_PROMPT_BYTES {
            return Err(SubagentMetadataError::InvalidPrompt);
        }
        let id = SubagentPresetId::new(id)?;
        Ok(Self {
            configuration_key: id.clone(),
            id,
            display,
            description: None,
            instructions,
            provider,
            seed,
            continuation,
            config: crate::SubagentConfig::default(),
        })
    }

    /// Attach optional catalog guidance describing when to select this agent.
    ///
    /// # Errors
    /// Empty, oversized or control-bearing description.
    pub fn with_description(
        mut self,
        description: Option<String>,
    ) -> Result<Self, SubagentMetadataError> {
        if description.as_ref().is_some_and(|text| {
            text.trim().is_empty()
                || text.len() > 4096
                || text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
        }) {
            return Err(SubagentMetadataError::InvalidLabel);
        }
        self.description = description;
        Ok(self)
    }

    /// Optional guidance for selecting this preset.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Attach validated execution controls.
    ///
    /// # Errors
    /// Invalid policy fields.
    pub fn with_config(mut self, config: crate::SubagentConfig) -> Result<Self, SubagentError> {
        config
            .validate()
            .map_err(|reason| SubagentError::new(SubagentErrorCode::Refused, reason))?;
        self.config = config;
        Ok(self)
    }

    /// Immutable child execution controls.
    #[must_use]
    pub fn config(&self) -> &crate::SubagentConfig {
        &self.config
    }

    /// Stable id.
    #[must_use]
    pub const fn id(&self) -> &SubagentPresetId {
        &self.id
    }

    /// Clone this preset under a validated alias (for scope precedence).
    ///
    /// # Errors
    /// Invalid alias id.
    pub fn with_id(mut self, id: impl Into<String>) -> Result<Self, SubagentMetadataError> {
        self.id = SubagentPresetId::new(id)?;
        Ok(self)
    }

    /// Human label.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Instructions placed in the child system layer.
    #[must_use]
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Optional exact provider selection.
    #[must_use]
    pub const fn provider(&self) -> Option<&SubagentProviderId> {
        self.provider.as_ref()
    }

    /// Default context seed.
    #[must_use]
    pub const fn seed(&self) -> SubagentSeed {
        self.seed
    }

    /// Default continuation mode.
    #[must_use]
    pub const fn continuation(&self) -> SubagentContinuation {
        self.continuation
    }
}

/// Stable failure classes every provider shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentErrorCode {
    /// The exact pending occurrence was already admitted by another turn.
    AlreadyConsumed,
    /// The request asks for a combination this provider does not prove.
    Unsupported,
    /// The nesting limit or another authority gate refused the request.
    Refused,
    /// The named child is unknown or already settled.
    Unknown,
    /// The caller cancelled before settlement.
    Cancelled,
    /// The child run failed.
    Failed,
}

impl SubagentErrorCode {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyConsumed => "already_consumed",
            Self::Unsupported => "unsupported",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// One bounded, body-free delegation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentError {
    code: SubagentErrorCode,
    message: String,
}

impl SubagentError {
    /// Construct a classified failure with bounded safe text.
    #[must_use]
    pub fn new(code: SubagentErrorCode, message: impl Into<String>) -> Self {
        let mut message = message.into();
        message.truncate(512);
        let message = message.replace(['\n', '\r'], " ");
        Self { code, message }
    }

    /// Stable class.
    #[must_use]
    pub const fn code(&self) -> SubagentErrorCode {
        self.code
    }

    /// Bounded one-line safe text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SubagentError {}

/// A delegated child that survived its first turn.
#[async_trait]
pub trait SubagentHandle: Send + Sync {
    /// Stable child identity.
    fn id(&self) -> &SubagentId;

    /// Human label recorded at delegation time.
    fn label(&self) -> &str;

    /// Run one follow-up turn and resolve with its final text.
    ///
    /// # Errors
    /// Cancellation, refusal, or child-turn failure.
    async fn send(
        &self,
        text: &str,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError>;

    /// Consume an exact inbox message under an owned job, without appending its text twice.
    async fn run_pending(
        &self,
        _id: &heycode_session::InboxMessageId,
        _cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        Err(SubagentError::new(
            SubagentErrorCode::Unsupported,
            "child does not support exact inbox turns",
        ))
    }

    /// Run with the registry's invocation identity. Adapters without a local
    /// inventory retain their ordinary send semantics.
    async fn send_in_job(
        &self,
        text: &str,
        _job: &JobId,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.send(text, cancellation).await
    }

    /// Dispatch an exact pending occurrence with its invocation identity.
    async fn run_pending_in_job(
        &self,
        id: &heycode_session::InboxMessageId,
        _job: &JobId,
        cancellation: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.run_pending(id, cancellation).await
    }

    /// Admit an exact durable mail occurrence. Providers without an inbox refuse explicitly.
    fn deliver_mail(&self, _id: &str, _text: &str) -> Result<bool, SubagentError> {
        Err(SubagentError::new(
            SubagentErrorCode::Unsupported,
            "child does not support durable mail delivery",
        ))
    }

    /// Consume pending inbox input under the child's existing turn gate.
    /// The caller must own this work through JobRegistry.
    ///
    /// # Errors
    /// Unsupported adapters, cancellation, or provider failure.
    async fn run_mail(&self, _cancellation: CancellationToken) -> Result<(), SubagentError> {
        Err(SubagentError::new(
            SubagentErrorCode::Unsupported,
            "child does not support inbox turns",
        ))
    }

    /// Cancel an in-flight turn without closing the child. Returns whether a
    /// turn was actually running.
    fn interrupt(&self) -> bool;

    /// Settle and release the child. Idempotent.
    ///
    /// # Errors
    /// Settlement failure.
    async fn close(&self, cancellation: CancellationToken) -> Result<(), SubagentError>;
}

/// Result of one delegation.
pub struct SubagentStarted {
    /// Child identity, quotable by the model.
    pub id: SubagentId,
    /// Final text of the child's first turn.
    pub text: String,
    /// Present exactly when the request asked for, and the provider granted,
    /// [`SubagentContinuation::Continuable`].
    pub handle: Option<Arc<dyn SubagentHandle>>,
}

impl std::fmt::Debug for SubagentStarted {
    /// Child text can be arbitrary model output, so `Debug` reports only its
    /// length rather than the body.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubagentStarted")
            .field("id", &self.id)
            .field("text_len", &self.text.len())
            .field("continuable", &self.handle.is_some())
            .finish()
    }
}

/// Delegation for one execution class.
#[async_trait]
pub trait SubagentProvider: Send + Sync {
    /// Static identity and tri-state evidence.
    fn descriptor(&self) -> &SubagentProviderDescriptor;

    /// Whether this provider enforces separate instructions and custom configuration.
    fn supports_configuration(&self) -> bool {
        false
    }

    /// Whether this provider's children run in THIS process, under the
    /// parent's own `seam/pre_tool` waterfall and approval policy.
    ///
    /// The default is `false`, so a provider that drives an agent elsewhere —
    /// an external CLI, a container, another machine — is never assumed to
    /// carry the parent's guards with it. Only a provider that literally hands
    /// the parent's seam to the child may override this to `true`; getting it
    /// wrong turns every ambient tool guard, plan mode included, into a
    /// suggestion.
    fn inherits_parent_tool_guards(&self) -> bool {
        false
    }

    /// Check current installation/authentication readiness; registration alone is not proof.
    async fn readiness(
        &self,
        cancellation: CancellationToken,
    ) -> Result<SubagentReadiness, SubagentError> {
        if cancellation.is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "readiness cancelled",
            ));
        }
        Ok(SubagentReadiness::Unknown)
    }

    /// Run one delegation to its first settlement.
    ///
    /// Implementations must refuse a request whose seed or continuation this
    /// provider does not prove, rather than downgrading it.
    ///
    /// # Errors
    /// Unsupported combination, authority refusal, cancellation, or child
    /// failure.
    async fn start(
        &self,
        request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError>;
}

/// Ambient policy consulted before delegating to a provider whose children do
/// NOT inherit the parent's tool guards.
///
/// A guard mounted on `seam/pre_tool` binds this agent's own tool calls. It
/// cannot reach a child running as a separate agent, so a state that must hold
/// for the whole session — plan mode is the shipped one — registers here to
/// refuse the delegation itself.
pub trait DelegationGate: Send + Sync {
    /// Why delegating to `provider` is refused right now, or `None` to allow.
    ///
    /// Called only for providers reporting
    /// [`SubagentProvider::inherits_parent_tool_guards`] as `false`.
    fn refuse_unguarded_delegation(&self, provider: &SubagentProviderId) -> Option<String>;
}

type PresetMap = std::collections::BTreeMap<SubagentPresetId, (SubagentPreset, Arc<()>)>;
type SharedPresetMap = Arc<std::sync::Mutex<PresetMap>>;

#[derive(Clone)]
struct SubagentJobHost {
    token: Arc<()>,
    agent: std::sync::Weak<Agent>,
    jobs: Arc<JobRegistry>,
}

/// Effect-owned registry of delegation providers plus the live continuable
/// children they produced.
///
/// Registration and every live child are removed on disposal, so context
/// shutdown cannot leave an orphaned child agent holding a durable session.
pub struct SubagentRegistry {
    workspace_paused: std::sync::Mutex<bool>,
    workspace_closed: std::sync::atomic::AtomicBool,
    providers: Arc<std::sync::Mutex<Vec<SubagentProviderEntry>>>,
    presets: SharedPresetMap,
    fallback_presets: SharedPresetMap,
    job_host: Arc<std::sync::Mutex<Option<SubagentJobHost>>>,
    children: std::sync::Mutex<std::collections::BTreeMap<SubagentId, OwnedSubagentHandle>>,
    authority_token: Arc<()>,
    message_aliases: std::sync::Mutex<std::collections::BTreeMap<(String, String), String>>,
    lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookSlot,
    gate: std::sync::Mutex<Option<Arc<dyn DelegationGate>>>,
    tasks: std::sync::Mutex<
        std::collections::BTreeMap<SubagentId, Arc<crate::task_inventory::TaskRecord>>,
    >,
    history_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    next_task: std::sync::atomic::AtomicU64,
    pub(crate) budget: Arc<crate::subagent_budget::SubagentBudget>,
    native_observers: Arc<NativeChildObservers>,
    archived: std::sync::Mutex<std::collections::BTreeMap<SubagentId, OwnedSubagentHandle>>,
}

struct SubagentProviderEntry {
    provider: Arc<dyn SubagentProvider>,
    token: Arc<()>,
}

struct OwnedSubagentHandle {
    closing: bool,
    owner: SubagentId,
    handle: Arc<dyn SubagentHandle>,
}

pub(crate) struct SubagentWorkspacePause(Arc<SubagentRegistry>);
impl Drop for SubagentWorkspacePause {
    fn drop(&mut self) {
        if let Ok(mut paused) = self.0.workspace_paused.lock() {
            *paused = false;
        }
    }
}
impl SubagentRegistry {
    pub(crate) fn close_workspace_admission(&self) {
        self.workspace_closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.dispose();
    }
    /// Freeze new task/restore admission only when no running or resumable child
    /// can retain a previous workspace generation. Archived handles count too.
    pub(crate) fn pause_workspace(
        self: &Arc<Self>,
    ) -> Result<SubagentWorkspacePause, SubagentError> {
        let refused = || {
            SubagentError::new(
                SubagentErrorCode::Refused,
                "Active or resumable child agents retain workspace authority; close them before changing workspace",
            )
        };
        let mut paused = self.workspace_paused.lock().map_err(|_| refused())?;
        if *paused
            || self
                .workspace_closed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(refused());
        }
        let tasks = self.tasks.lock().map_err(|_| refused())?;
        let children = self.children.lock().map_err(|_| refused())?;
        let archived = self.archived.lock().map_err(|_| refused())?;
        if !children.is_empty()
            || !archived.is_empty()
            || tasks.values().any(|record| record.read().state.active())
        {
            return Err(refused());
        }
        *paused = true;
        Ok(SubagentWorkspacePause(self.clone()))
    }
}

impl Default for SubagentRegistry {
    fn default() -> Self {
        Self {
            workspace_paused: std::sync::Mutex::new(false),
            workspace_closed: std::sync::atomic::AtomicBool::new(false),
            providers: Arc::new(std::sync::Mutex::new(Vec::new())),
            presets: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            fallback_presets: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            job_host: Arc::new(std::sync::Mutex::new(None)),
            children: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            authority_token: Arc::new(()),
            message_aliases: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookSlot::default(),
            gate: std::sync::Mutex::new(None),
            tasks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            history_root: std::sync::Mutex::new(None),
            next_task: std::sync::atomic::AtomicU64::new(0),
            budget: Arc::new(crate::subagent_budget::SubagentBudget::new(
                crate::SubagentBudgetLimits::default(),
            )),
            native_observers: Arc::new(std::sync::Mutex::new(Vec::new())),
            archived: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

impl std::fmt::Debug for SubagentRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubagentRegistry")
            .field(
                "providers",
                &self.providers.lock().map(|rows| rows.len()).unwrap_or(0),
            )
            .field(
                "children",
                &self.children.lock().map(|rows| rows.len()).unwrap_or(0),
            )
            .field(
                "presets",
                &self.presets.lock().map(|rows| rows.len()).unwrap_or(0),
            )
            .field(
                "background_jobs",
                &self
                    .job_host
                    .lock()
                    .map(|host| host.is_some())
                    .unwrap_or(false),
            )
            .field(
                "delegation_gate",
                &self.gate.lock().map(|gate| gate.is_some()).unwrap_or(false),
            )
            .finish()
    }
}

impl SubagentRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the product hook adapter for subagent lifecycle events.
    ///
    /// # Errors
    /// Another adapter already owns the slot or its state is unavailable.
    pub fn attach_lifecycle_hooks(
        &self,
        context: &heycode_core::Context,
        port: Arc<dyn crate::LifecycleHookPort>,
    ) -> Result<(), crate::LifecycleHookAttachmentError> {
        self.lifecycle_hooks.install(context, port)
    }

    pub(crate) fn lifecycle_hook_slot(&self) -> crate::lifecycle_hooks::LifecycleHookSlot {
        self.lifecycle_hooks.clone()
    }

    /// Install the ambient gate consulted before every delegation to a
    /// provider that does not inherit the parent's tool guards.
    ///
    /// One registry holds exactly one gate: a second claimant would silently
    /// decide which session-wide policy is enforced, so it is refused.
    ///
    /// # Errors
    /// A gate is already installed, or the registry state is poisoned.
    pub fn attach_delegation_gate(
        &self,
        gate: Arc<dyn DelegationGate>,
    ) -> Result<(), SubagentError> {
        let mut slot = self.gate.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
        })?;
        if slot.is_some() {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "a delegation gate is already installed",
            ));
        }
        *slot = Some(gate);
        Ok(())
    }

    /// Ask the installed gate whether an unguarded delegation is refused.
    ///
    /// A poisoned slot refuses: the gate exists to withhold delegation, so its
    /// unavailability may never be read as permission.
    fn refuse_unguarded_delegation(&self, provider: &SubagentProviderId) -> Option<String> {
        match self.gate.lock() {
            Ok(slot) => slot
                .as_ref()
                .and_then(|gate| gate.refuse_unguarded_delegation(provider)),
            Err(_) => Some("delegation gate is unavailable".to_owned()),
        }
    }

    /// Mint the root authority for one host-owned session.
    ///
    /// The returned token is registry-specific and is the only value from
    /// which nested owner/depth authorities can derive.
    #[must_use]
    pub fn root_authority(&self, owner: SubagentId) -> SubagentAuthority {
        SubagentAuthority::bound(self.authority_token.clone(), owner)
    }

    /// Whether this registry minted `authority`.
    ///
    /// This proves registry provenance only; owner-specific operations still
    /// apply their own roster/child checks.
    #[must_use]
    pub fn recognizes_authority(&self, authority: &SubagentAuthority) -> bool {
        authority.belongs_to(&self.authority_token)
    }

    /// Register one provider.
    ///
    /// # Errors
    /// A provider with the same id is already registered, or the registry
    /// state is poisoned. Duplicate registration fails loud rather than
    /// shadowing an existing provider.
    pub fn register(&self, provider: Arc<dyn SubagentProvider>) -> Result<(), SubagentError> {
        let _token = self.insert_provider(provider)?;
        Ok(())
    }

    /// Register one provider and return its exact ownership handle.
    ///
    /// # Errors
    /// Duplicate id or poisoned registry state.
    pub fn register_owned(
        &self,
        provider: Arc<dyn SubagentProvider>,
    ) -> Result<SubagentProviderRegistration, SubagentError> {
        let (id, token) = self.insert_provider(provider)?;
        Ok(SubagentProviderRegistration {
            providers: Arc::downgrade(&self.providers),
            id,
            token,
            active: true,
        })
    }

    fn insert_provider(
        &self,
        provider: Arc<dyn SubagentProvider>,
    ) -> Result<(SubagentProviderId, Arc<()>), SubagentError> {
        let mut providers = self.providers.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
        })?;
        let id = provider.descriptor().id().clone();
        if providers
            .iter()
            .any(|existing| existing.provider.descriptor().id() == &id)
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                format!("subagent provider `{id}` is already registered"),
            ));
        }
        let token = Arc::new(());
        providers.push(SubagentProviderEntry {
            provider,
            token: token.clone(),
        });
        Ok((id, token))
    }

    /// Remove one provider by id, returning whether a row was removed.
    pub fn remove(&self, id: &SubagentProviderId) -> bool {
        let Ok(mut providers) = self.providers.lock() else {
            return false;
        };
        let before = providers.len();
        providers.retain(|existing| existing.provider.descriptor().id() != id);
        providers.len() != before
    }

    /// Registered descriptors, in registration order.
    #[must_use]
    pub fn descriptors(&self) -> Vec<SubagentProviderDescriptor> {
        self.providers
            .lock()
            .map(|providers| {
                providers
                    .iter()
                    .map(|entry| entry.provider.descriptor().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Register one declarative preset and return its exact disposer.
    ///
    /// # Errors
    /// Duplicate ids or poisoned state fail before publication.
    pub fn register_preset_owned(
        &self,
        preset: SubagentPreset,
    ) -> Result<SubagentPresetRegistration, SubagentError> {
        Self::register_preset_in(&self.presets, preset)
    }

    /// Register a token-owned built-in fallback. File/published presets with
    /// the same id shadow it; withdrawing the override restores this row.
    ///
    /// # Errors
    /// Duplicate fallback ids or poisoned state.
    pub fn register_fallback_preset_owned(
        &self,
        preset: SubagentPreset,
    ) -> Result<SubagentPresetRegistration, SubagentError> {
        Self::register_preset_in(&self.fallback_presets, preset)
    }

    fn register_preset_in(
        map: &SharedPresetMap,
        preset: SubagentPreset,
    ) -> Result<SubagentPresetRegistration, SubagentError> {
        let id = preset.id().clone();
        let token = Arc::new(());
        let mut presets = map.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
        })?;
        if presets.contains_key(&id) {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                format!("subagent preset `{id}` is already registered"),
            ));
        }
        presets.insert(id.clone(), (preset, token.clone()));
        Ok(SubagentPresetRegistration {
            presets: Arc::downgrade(map),
            id,
            token,
            active: true,
        })
    }

    /// Atomically replace a caller's exact owned preset generation. Existing
    /// children keep their resolved configuration; future spawns see the new set.
    ///
    /// # Errors
    /// Duplicates, foreign claims or unavailable registry leave the old set intact.
    pub fn replace_presets_owned(
        &self,
        owned: &mut Vec<SubagentPresetRegistration>,
        next: Vec<SubagentPreset>,
    ) -> Result<(), SubagentError> {
        let mut rows = self.presets.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "preset registry unavailable")
        })?;
        let mut ids = std::collections::BTreeSet::new();
        for preset in &next {
            if !ids.insert(preset.id().clone())
                || rows.get(preset.id()).is_some_and(|(_, token)| {
                    !owned
                        .iter()
                        .any(|old| old.id == *preset.id() && Arc::ptr_eq(&old.token, token))
                })
            {
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "preset generation conflicts with an existing owner",
                ));
            }
        }
        for old in owned.iter() {
            if rows
                .get(&old.id)
                .is_some_and(|(_, token)| Arc::ptr_eq(token, &old.token))
            {
                rows.remove(&old.id);
            }
        }
        let mut replacements = Vec::new();
        for preset in next {
            let id = preset.id().clone();
            let token = Arc::new(());
            rows.insert(id.clone(), (preset, token.clone()));
            replacements.push(SubagentPresetRegistration {
                presets: Arc::downgrade(&self.presets),
                id,
                token,
                active: true,
            });
        }
        drop(rows);
        *owned = replacements;
        Ok(())
    }

    /// Ordered declarative preset snapshot.
    #[must_use]
    pub fn presets(&self) -> Vec<SubagentPreset> {
        let mut effective = self
            .fallback_presets
            .lock()
            .map(|rows| {
                rows.iter()
                    .map(|(id, (preset, _))| (id.clone(), preset.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        if let Ok(rows) = self.presets.lock() {
            effective.extend(
                rows.iter()
                    .map(|(id, (preset, _))| (id.clone(), preset.clone())),
            );
        }
        effective.into_values().collect()
    }

    /// Exact preset lookup.
    #[must_use]
    pub fn preset(&self, id: &str) -> Option<SubagentPreset> {
        let id = SubagentPresetId::new(id).ok()?;
        self.presets
            .lock()
            .ok()?
            .get(&id)
            .map(|(preset, _)| preset.clone())
            .or_else(|| {
                self.fallback_presets
                    .lock()
                    .ok()?
                    .get(&id)
                    .map(|(preset, _)| preset.clone())
            })
    }

    /// Attach the Agent/job owner used by background `task` calls.
    ///
    /// The registration is an exact Context effect and stores only a weak
    /// Agent reference, avoiding a service cycle. A second live host is
    /// refused rather than shadowed.
    ///
    /// # Errors
    /// Duplicate attachment or poisoned state.
    pub fn attach_job_host(
        self: &Arc<Self>,
        context: &heycode_core::Context,
        agent: &Arc<Agent>,
        jobs: Arc<JobRegistry>,
    ) -> Result<(), SubagentError> {
        let token = Arc::new(());
        let mut host = self.job_host.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
        })?;
        if host.is_some() {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "subagent background-job host is already attached",
            ));
        }
        *host = Some(SubagentJobHost {
            token: token.clone(),
            agent: Arc::downgrade(agent),
            jobs: jobs.clone(),
        });
        drop(host);
        let owner = Arc::downgrade(&self.job_host);
        context.effect(move || {
            let Some(owner) = owner.upgrade() else {
                return;
            };
            let Ok(mut host) = owner.lock() else {
                return;
            };
            if host
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.token, &token))
            {
                *host = None;
            }
        });
        let recipient = SubagentId::new(
            agent
                .session()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .id()
                .to_string(),
        )
        .map_err(|_| SubagentError::new(SubagentErrorCode::Failed, "invalid root recipient"))?;
        let registry = Arc::downgrade(self);
        let weak_jobs = Arc::downgrade(&jobs);
        let weak_agent = Arc::downgrade(agent);
        *agent.inbox_waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(move || {
            if let (Some(registry), Some(jobs)) = (registry.upgrade(), weak_jobs.upgrade())
                && let Err(error) = registry.wake_native_parent(&recipient, &jobs)
                && let Some(agent) = weak_agent.upgrade()
            {
                agent.ui().emit(crate::UiEvent::Error {
                    message: error.to_string(),
                });
            }
        }));
        let weak_agent = Arc::downgrade(agent);
        context.effect(move || {
            if let Some(agent) = weak_agent.upgrade() {
                *agent.inbox_waker.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        });
        agent.announce_settled_inbox();
        Ok(())
    }

    /// Whether background delegation has a durable Agent/job owner.
    #[must_use]
    pub fn background_available(&self) -> bool {
        self.job_host
            .lock()
            .map(|host| {
                host.as_ref()
                    .is_some_and(|host| host.agent.strong_count() > 0)
            })
            .unwrap_or(false)
    }

    /// Start one delegation as an effect-owned background job.
    ///
    /// The returned id is usable by `list_jobs`/`cancel_job`; settlement first
    /// commits a bounded notice through the durable inbox and only then changes
    /// the visible job state.
    ///
    /// # Errors
    /// Missing job host, invalid request, registry failure or spawn refusal.
    pub fn start_background(
        self: &Arc<Self>,
        request: SubagentRequest,
        delivery: InboxDelivery,
    ) -> Result<JobId, SubagentError> {
        self.start_background_task(request, delivery)
            .map(|(_, job)| job)
    }

    /// Admit a background child, returning its stable task and job IDs immediately.
    pub fn start_background_task(
        self: &Arc<Self>,
        mut request: SubagentRequest,
        delivery: InboxDelivery,
    ) -> Result<(SubagentId, JobId), SubagentError> {
        let host = self
            .job_host
            .lock()
            .map_err(|_| {
                SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
            })?
            .clone()
            .ok_or_else(|| {
                SubagentError::new(
                    SubagentErrorCode::Unsupported,
                    "subagent background jobs are unavailable",
                )
            })?;
        let task_id = self.admit_task(&mut request, CancellationToken::new())?;
        let record = request.task.clone().ok_or_else(|| {
            SubagentError::new(SubagentErrorCode::Failed, "task admission missing")
        })?;
        let label = request.label().to_owned();
        let sender_id = task_id.clone();
        let sender_name = label.clone();
        let run_record = record.clone();
        let registry = self.clone();
        let settle_jobs = host.jobs.clone();
        let recipient = record
            .parent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| host.agent.upgrade());
        let parent_id = request.authority().owner().clone();
        let spawned = host
            .jobs
            .spawn_coordinator(label, delivery, move |id, cancellation| async move {
                let result = match run_record.update(|row| row.job_id = Some(id.to_string())) {
                    Ok(()) => registry.start(request, cancellation.clone()).await,
                    Err(error) => Err(task_io_error(error)),
                };
                let (outcome, notice) = match result {
                    Ok(_) if cancellation.is_cancelled() => (
                        JobOutcome::Cancelled,
                        "subagent cancelled; continuable handle remains available".to_owned(),
                    ),
                    Ok(started) => {
                        let suffix = started.handle.as_ref().map_or_else(String::new, |_| {
                            format!(
                                "\n\n[task_id: {} — continue with agent_control(action=send)]",
                                started.id
                            )
                        });
                        (
                            JobOutcome::Completed,
                            bounded_job_notice(&format!("{}{suffix}", started.text)),
                        )
                    }
                    Err(error) if error.code() == SubagentErrorCode::Cancelled => {
                        (JobOutcome::Cancelled, "subagent cancelled".to_owned())
                    }
                    Err(error) => (
                        JobOutcome::Failed,
                        bounded_job_notice(&format!(
                            "subagent failed ({}): {}",
                            error.code().as_str(),
                            error.message()
                        )),
                    ),
                };
                let Ok(settlement) = JobSettlement::new(outcome, notice) else {
                    return;
                };
                let Some(agent) = recipient else {
                    return;
                };
                let source = agent.agent_completion_source(
                    &id,
                    sender_id.as_str(),
                    &sender_name,
                    parent_id.as_str(),
                    settlement.outcome(),
                );
                match agent
                    .settle_agent_job_with_retry(
                        &settle_jobs,
                        &id,
                        &settlement,
                        source,
                        cancellation.clone(),
                    )
                    .await
                {
                    Ok(crate::WakeDecision::Woke) => {
                        if let Err(error) = registry.wake_native_parent(&parent_id, &settle_jobs) {
                            agent.ui().emit(crate::UiEvent::Error {
                                message: error.to_string(),
                            });
                        }
                    }
                    Ok(_) => {}
                    Err(_) => agent.ui().emit(crate::UiEvent::Error {
                        message: "background subagent settlement could not be committed".to_owned(),
                    }),
                }
            })
            .map_err(|_| {
                SubagentError::new(
                    SubagentErrorCode::Failed,
                    "subagent background job could not start",
                )
            });
        match spawned {
            Ok(job) => {
                record
                    .update(|row| row.job_id = Some(job.to_string()))
                    .map_err(task_io_error)?;
                Ok((task_id, job))
            }
            Err(error) => {
                record
                    .finish(crate::TaskState::Failed, error.message())
                    .map_err(task_io_error)?;
                Err(error)
            }
        }
    }

    /// The first registered provider proving it can serve this request.
    ///
    /// Selection is deterministic registration order, never "last wins".
    #[must_use]
    pub fn select(&self, request: &SubagentRequest) -> Option<Arc<dyn SubagentProvider>> {
        self.providers
            .lock()
            .ok()?
            .iter()
            .find(|entry| {
                request
                    .provider()
                    .is_none_or(|wanted| entry.provider.descriptor().id() == wanted)
                    && entry.provider.descriptor().supports(request)
                    && ((request.instructions().is_empty()
                        && request.config() == &crate::SubagentConfig::default())
                        || entry.provider.supports_configuration())
            })
            .map(|entry| entry.provider.clone())
    }

    /// Run one delegation through the selected provider, retaining any
    /// continuable child.
    ///
    /// # Errors
    /// No provider proves the requested combination, or the provider failed.
    pub async fn start(
        &self,
        mut request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        if request.task.is_none() {
            self.admit_task(&mut request, cancellation.clone())?;
        }
        let record = request.task.clone().ok_or_else(|| {
            SubagentError::new(SubagentErrorCode::Failed, "task admission missing")
        })?;
        if record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_cancelled()
        {
            cancellation.cancel();
        }
        *record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = cancellation.clone();
        record
            .update(|row| row.state = crate::TaskState::Running)
            .map_err(task_io_error)?;
        let mut execution = TaskExecutionLease {
            record: record.clone(),
            settled: false,
        };
        use futures::FutureExt;
        let result = std::panic::AssertUnwindSafe(self.start_inner(request, cancellation.clone()))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                cancellation.cancel();
                Err(SubagentError::new(
                    SubagentErrorCode::Failed,
                    "subagent provider panicked",
                ))
            });
        let (state, output) = match &result {
            Ok(started) => (
                if started.handle.is_some() {
                    crate::TaskState::Idle
                } else if cancellation.is_cancelled() {
                    crate::TaskState::Cancelled
                } else {
                    crate::TaskState::Completed
                },
                started.text.as_str(),
            ),
            Err(error) => (
                if error.code() == SubagentErrorCode::Cancelled || cancellation.is_cancelled() {
                    crate::TaskState::Cancelled
                } else {
                    crate::TaskState::Failed
                },
                error.message(),
            ),
        };
        let diagnostic = (state == crate::TaskState::Failed)
            .then(|| record.read().terminal_diagnostic)
            .flatten();
        record
            .finish_with_diagnostic(state, output, diagnostic)
            .map_err(task_io_error)?;
        execution.settled = true;
        *record.parent.lock().unwrap_or_else(|e| e.into_inner()) = None;
        result
    }

    async fn start_inner(
        &self,
        request: SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        if !request.authority().belongs_to(&self.authority_token) {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "subagent request has no authority for this registry",
            ));
        }
        if request.continuation == SubagentContinuation::Continuable
            && !request.authority().may_retain_children()
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "one-shot subagent authority cannot create a continuable child",
            ));
        }
        let owner = request.authority().owner().clone();
        let continuation = request.continuation;
        let hook_payload = request.prompt().to_owned();
        let provider = self.select(&request).ok_or_else(|| {
            SubagentError::new(
                SubagentErrorCode::Unsupported,
                "no subagent provider supports the requested seed, continuation and custom-agent configuration",
            )
        })?;
        // Before any child exists: a provider that does not carry the parent's
        // `seam/pre_tool` guards into its child is the one hole those guards
        // cannot cover themselves.
        if !provider.inherits_parent_tool_guards()
            && let Some(reason) = self.refuse_unguarded_delegation(provider.descriptor().id())
        {
            return Err(SubagentError::new(SubagentErrorCode::Refused, reason));
        }
        let pre_hook = self
            .lifecycle_hooks
            .run(
                crate::LifecycleHookRequest::new(
                    crate::LifecycleHookPhase::Pre,
                    crate::LifecycleHookEvent::Subagent,
                    hook_payload.clone(),
                ),
                cancellation.child_token(),
            )
            .await;
        if pre_hook.decision() == crate::LifecycleHookDecision::Refuse {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "subagent start refused by a lifecycle hook",
            ));
        }
        let record = request.task.clone();
        // External provider internals do not use Agent request admission; bound the
        // entire delegated operation instead. Native requests release permits before tools.
        let external_permit = if provider.inherits_parent_tool_guards() {
            None
        } else {
            Some(
                self.budget
                    .acquire(&cancellation, &cancellation)
                    .await
                    .map_err(|error| {
                        SubagentError::new(
                            if cancellation.is_cancelled() {
                                SubagentErrorCode::Cancelled
                            } else {
                                SubagentErrorCode::Refused
                            },
                            error.to_string(),
                        )
                    })?,
            )
        };
        let mut started = provider.start(request, cancellation.clone()).await?;
        drop(external_permit);
        match (continuation, started.handle.as_ref()) {
            (SubagentContinuation::OneShot, Some(handle)) => {
                let _ = handle.close(CancellationToken::new()).await;
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "one-shot subagent provider returned a continuable handle",
                ));
            }
            (SubagentContinuation::Continuable, None) => {
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "continuable subagent provider returned no handle",
                ));
            }
            (_, Some(handle)) if handle.id() != &started.id => {
                let _ = handle.close(CancellationToken::new()).await;
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "subagent provider returned mismatched child identity",
                ));
            }
            _ => {}
        }
        if let Some(record) = record {
            let id = SubagentId::new(record.read().id).map_err(|error| {
                SubagentError::new(SubagentErrorCode::Failed, error.to_string())
            })?;
            if record.read().session_id.is_none() {
                record
                    .update(|row| row.session_id = Some(started.id.to_string()))
                    .map_err(task_io_error)?;
            }
            if let Some(handle) = started.handle.take() {
                // Reject a provider reusing one live backend conversation for two children.
                let duplicate = self
                    .tasks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .any(|other| {
                        let row = other.read();
                        row.id != id.as_str()
                            && row.provider == record.read().provider
                            && row.session_id == record.read().session_id
                            && (row.state.active()
                                || row.state == crate::TaskState::Idle
                                || row.state == crate::TaskState::Closed)
                    });
                if duplicate {
                    let _ = handle.close(CancellationToken::new()).await;
                    return Err(SubagentError::new(
                        SubagentErrorCode::Refused,
                        "subagent provider reused a live child identity",
                    ));
                }
                started.handle = Some(Arc::new(AliasedHandle {
                    id: id.clone(),
                    inner: handle,
                    record,
                }));
            }
            started.id = id;
        }
        if let Some(handle) = started.handle.clone() {
            let duplicate = {
                let mut children = self.children.lock().map_err(|_| {
                    SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
                })?;
                if children.contains_key(&started.id) {
                    true
                } else {
                    children.insert(
                        started.id.clone(),
                        OwnedSubagentHandle {
                            closing: false,
                            owner,
                            handle: handle.clone(),
                        },
                    );
                    false
                }
            };
            if duplicate {
                let _ = handle.close(CancellationToken::new()).await;
                return Err(SubagentError::new(
                    SubagentErrorCode::Refused,
                    "subagent provider returned a duplicate child id",
                ));
            }
        }
        let _post_hook = self
            .lifecycle_hooks
            .run(
                crate::LifecycleHookRequest::new(
                    crate::LifecycleHookPhase::Post,
                    crate::LifecycleHookEvent::Subagent,
                    hook_payload,
                ),
                cancellation.child_token(),
            )
            .await;
        Ok(started)
    }

    /// Live continuable child owned by `authority` and matching `id`.
    ///
    /// Foreign and unknown ids are indistinguishable to the caller.
    #[must_use]
    pub fn child_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Option<Arc<dyn SubagentHandle>> {
        if !authority.belongs_to(&self.authority_token) {
            return None;
        }
        self.children
            .lock()
            .ok()?
            .get(id)
            .filter(|entry| &entry.owner == authority.owner() && !entry.closing)
            .map(|entry| entry.handle.clone())
    }

    /// Derive the authority carried by one live child owned by `authority`.
    /// Foreign and unknown ids remain indistinguishable.
    #[must_use]
    pub fn authority_for_child(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Option<SubagentAuthority> {
        self.child_for(authority, id)?;
        Some(authority.child(id.clone(), true))
    }

    /// Ordered `(id, label)` snapshot owned by `authority`.
    #[must_use]
    pub fn children_for(&self, authority: &SubagentAuthority) -> Vec<(SubagentId, String)> {
        if !authority.belongs_to(&self.authority_token) {
            return Vec::new();
        }
        self.children
            .lock()
            .map(|children| {
                children
                    .iter()
                    .filter(|(_, entry)| &entry.owner == authority.owner())
                    .map(|(id, entry)| (id.clone(), entry.handle.label().to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Number of all live continuable children, without exposing identities
    /// across owners.
    #[must_use]
    pub fn live_child_count(&self) -> usize {
        self.task_snapshots()
            .iter()
            .filter(|row| row.state.active() || row.state == crate::TaskState::Idle)
            .count()
    }

    /// Close and forget one live child. Returns whether a child was removed.
    ///
    /// # Errors
    /// The child's own settlement failed; the row is removed regardless, so a
    /// failed close cannot leave an unreachable child in the registry.
    pub async fn close_child_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
        cancellation: CancellationToken,
    ) -> Result<bool, SubagentError> {
        if !authority.belongs_to(&self.authority_token) {
            return Ok(false);
        }
        let handle = {
            let Ok(mut children) = self.children.lock() else {
                return Err(SubagentError::new(
                    SubagentErrorCode::Failed,
                    "subagent registry unavailable",
                ));
            };
            match children.get_mut(id) {
                Some(entry) if &entry.owner == authority.owner() => {
                    if entry.closing {
                        return Err(SubagentError::new(
                            SubagentErrorCode::Failed,
                            "child close is in progress or its outcome is uncertain",
                        ));
                    }
                    entry.closing = true;
                    Some(entry.handle.clone())
                }
                _ => None,
            }
        };
        let Some(handle) = handle else {
            return Ok(false);
        };
        let closed = handle.close(cancellation).await;
        if let Some(record) = self.task_record_for(authority, id) {
            record
                .update(|row| {
                    row.state = if closed.is_ok() {
                        crate::TaskState::Closed
                    } else {
                        crate::TaskState::Interrupted
                    }
                })
                .map_err(task_io_error)?;
        }
        closed?;
        self.children
            .lock()
            .map_err(|_| {
                SubagentError::new(SubagentErrorCode::Failed, "subagent registry unavailable")
            })?
            .remove(id);
        Ok(true)
    }

    /// Interrupt one live child's active turn.
    #[must_use]
    pub fn interrupt_for(&self, authority: &SubagentAuthority, id: &SubagentId) -> bool {
        if let Some(record) = self.task_record_for(authority, id)
            && record.read().state.active()
        {
            record
                .cancellation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel();
            let _ = record.update(|row| row.state = crate::TaskState::Cancelling);
            return true;
        }
        self.child_for(authority, id)
            .is_some_and(|handle| handle.interrupt())
    }

    /// Drop every provider and interrupt every live child.
    ///
    /// Used by the owning effect's disposer. Interrupting rather than awaiting
    /// close keeps disposal synchronous and non-blocking; each child's own
    /// cancellation then settles it.
    pub fn dispose(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for record in tasks.values() {
                record
                    .cancellation
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cancel();
            }
            tasks.clear();
        }
        if let Ok(mut archived) = self.archived.lock() {
            archived.clear();
        }
        if let Ok(mut observers) = self.native_observers.lock() {
            observers.clear();
        }
        if let Ok(mut providers) = self.providers.lock() {
            providers.clear();
        }
        if let Ok(mut presets) = self.presets.lock() {
            presets.clear();
        }
        if let Ok(mut presets) = self.fallback_presets.lock() {
            presets.clear();
        }
        if let Ok(mut host) = self.job_host.lock() {
            *host = None;
        }
        if let Ok(mut children) = self.children.lock() {
            for entry in children.values() {
                let _interrupted = entry.handle.interrupt();
            }
            children.clear();
        }
    }
}

/// Exact ownership handle for one late subagent provider.
pub struct SubagentProviderRegistration {
    providers: std::sync::Weak<std::sync::Mutex<Vec<SubagentProviderEntry>>>,
    id: SubagentProviderId,
    token: Arc<()>,
    active: bool,
}

impl Drop for SubagentProviderRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(providers) = self.providers.upgrade() else {
            return;
        };
        let Ok(mut providers) = providers.lock() else {
            return;
        };
        providers.retain(|entry| {
            entry.provider.descriptor().id() != &self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
    }
}

fn bounded_job_notice(value: &str) -> String {
    const LIMIT: usize = 7 * 1024;
    let mut notice = value.chars().take(LIMIT).collect::<String>();
    if notice.trim().is_empty() {
        notice = "subagent completed without text".to_owned();
    }
    notice
}

/// Exact ownership handle for one declarative subagent preset.
pub struct SubagentPresetRegistration {
    presets: std::sync::Weak<std::sync::Mutex<PresetMap>>,
    id: SubagentPresetId,
    token: Arc<()>,
    active: bool,
}

impl Drop for SubagentPresetRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(presets) = self.presets.upgrade() else {
            return;
        };
        let Ok(mut presets) = presets.lock() else {
            return;
        };
        let remove = presets
            .get(&self.id)
            .is_some_and(|(_, token)| Arc::ptr_eq(token, &self.token));
        if remove {
            presets.remove(&self.id);
        }
    }
}

fn kebab_case(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn task_io_error(error: std::io::Error) -> SubagentError {
    SubagentError::new(
        SubagentErrorCode::Failed,
        format!("task history persistence failed: {error}"),
    )
}
struct AliasedHandle {
    id: SubagentId,
    inner: Arc<dyn SubagentHandle>,
    record: Arc<crate::task_inventory::TaskRecord>,
}
impl AliasedHandle {
    fn finish_run(
        &self,
        result: &Result<String, SubagentError>,
        stage: &str,
    ) -> Result<(), SubagentError> {
        let state = match result {
            Ok(_) => crate::TaskState::Idle,
            Err(error) if error.code() == SubagentErrorCode::Cancelled => {
                crate::TaskState::Cancelled
            }
            Err(_) => crate::TaskState::Failed,
        };
        let retained = self.record.read().terminal_diagnostic;
        let diagnostic = result
            .as_ref()
            .err()
            .filter(|_| state == crate::TaskState::Failed)
            .map(|error| crate::TaskDiagnostic {
                message: error.message().to_owned(),
                code: Some(error.code().as_str().to_owned()),
                stage: Some(stage.to_owned()),
                ..crate::TaskDiagnostic::default()
            });
        let diagnostic = retained.or(diagnostic);
        self.record
            .finish_with_diagnostic(
                state,
                result.as_deref().unwrap_or_else(|error| error.message()),
                diagnostic,
            )
            .map_err(task_io_error)
    }
}
impl AliasedHandle {
    async fn send_owned(
        &self,
        text: &str,
        job: Option<&JobId>,
        token: CancellationToken,
    ) -> Result<String, SubagentError> {
        let _turn = tokio::select! {
            biased;
            () = token.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "message cancelled while queued")),
            turn = self.record.turns.lock() => turn,
        };
        if self
            .record
            .closing
            .load(std::sync::atomic::Ordering::SeqCst)
            || matches!(
                self.record.read().state,
                crate::TaskState::Closed
                    | crate::TaskState::Cancelled
                    | crate::TaskState::Interrupted
            )
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "task is closed",
            ));
        }
        *self
            .record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = token.clone();
        self.record
            .update(|row| {
                row.state = crate::TaskState::Running;
                if let Some(job) = job {
                    row.job_id = Some(job.to_string());
                }
            })
            .map_err(task_io_error)?;
        let result = self.inner.send(text, token.clone()).await;
        self.finish_run(&result, "follow_up")?;
        result
    }
    async fn run_pending_owned(
        &self,
        id: &heycode_session::InboxMessageId,
        job: Option<&JobId>,
        token: CancellationToken,
    ) -> Result<String, SubagentError> {
        let _turn = loop {
            let changed = self.record.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let turn = tokio::select! {
                biased;
                () = token.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "message cancelled while queued")),
                turn = self.record.turns.lock() => turn,
            };
            let earlier = self
                .record
                .native
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade()
                .is_some_and(|native| {
                    let session = native.session().lock().unwrap_or_else(|e| e.into_inner());
                    let inbox = session.inbox();
                    let queue = if inbox.next_turn().iter().any(|message| message.id() == id) {
                        inbox.next_turn()
                    } else {
                        inbox.next_step()
                    };
                    queue
                        .iter()
                        .take_while(|message| message.id() != id)
                        .any(|message| message.delivery() != InboxDelivery::Inject)
                        && queue.iter().any(|message| message.id() == id)
                });
            if !earlier {
                break turn;
            }
            drop(turn);
            tokio::select! {
                () = &mut changed => {},
                () = token.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "message cancelled while waiting for earlier input")),
            }
        };
        if self
            .record
            .closing
            .load(std::sync::atomic::Ordering::SeqCst)
            || matches!(
                self.record.read().state,
                crate::TaskState::Closed
                    | crate::TaskState::Cancelled
                    | crate::TaskState::Interrupted
            )
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "task is closed",
            ));
        }
        let prior = self.record.read();
        if let Some(native) = self
            .record
            .native
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
        {
            let session = native.session().lock().unwrap_or_else(|e| e.into_inner());
            if !session
                .inbox()
                .next_turn()
                .iter()
                .chain(session.inbox().next_step())
                .any(|message| message.id() == id)
            {
                return Err(SubagentError::new(
                    SubagentErrorCode::AlreadyConsumed,
                    "message already consumed",
                ));
            }
        }
        *self
            .record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = token.clone();
        self.record
            .update(|row| {
                row.state = crate::TaskState::Running;
                if let Some(job) = job {
                    row.job_id = Some(job.to_string());
                }
            })
            .map_err(task_io_error)?;
        let result = self.inner.run_pending(id, token).await;
        if result
            .as_ref()
            .is_err_and(|error| error.code() == SubagentErrorCode::AlreadyConsumed)
        {
            self.record
                .update(|row| {
                    row.state = prior.state;
                    row.job_id = prior.job_id;
                    row.output = prior.output;
                    row.output_truncated = prior.output_truncated;
                    row.terminal_diagnostic = prior.terminal_diagnostic;
                })
                .map_err(task_io_error)?;
        } else {
            self.finish_run(&result, "follow_up")?;
        }
        result
    }
}
#[async_trait]
impl SubagentHandle for AliasedHandle {
    fn id(&self) -> &SubagentId {
        &self.id
    }
    fn label(&self) -> &str {
        self.inner.label()
    }
    async fn send(&self, text: &str, token: CancellationToken) -> Result<String, SubagentError> {
        self.send_owned(text, None, token).await
    }
    async fn send_in_job(
        &self,
        text: &str,
        job: &JobId,
        token: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.send_owned(text, Some(job), token).await
    }
    async fn run_pending(
        &self,
        id: &heycode_session::InboxMessageId,
        token: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.run_pending_owned(id, None, token).await
    }
    async fn run_pending_in_job(
        &self,
        id: &heycode_session::InboxMessageId,
        job: &JobId,
        token: CancellationToken,
    ) -> Result<String, SubagentError> {
        self.run_pending_owned(id, Some(job), token).await
    }
    fn deliver_mail(&self, id: &str, text: &str) -> Result<bool, SubagentError> {
        self.inner.deliver_mail(id, text)
    }
    async fn run_mail(&self, token: CancellationToken) -> Result<(), SubagentError> {
        let _turn = tokio::select! {
            biased;
            () = token.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "mail cancelled while queued")),
            turn = self.record.turns.lock() => turn,
        };
        if self
            .record
            .closing
            .load(std::sync::atomic::Ordering::SeqCst)
            || matches!(
                self.record.read().state,
                crate::TaskState::Closed
                    | crate::TaskState::Cancelled
                    | crate::TaskState::Interrupted
            )
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "task is closed",
            ));
        }
        *self
            .record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = token.clone();
        self.record
            .update(|row| row.state = crate::TaskState::Running)
            .map_err(task_io_error)?;
        let result = self.inner.run_mail(token).await;
        self.finish_run(
            &result
                .as_ref()
                .map(|()| self.record.read().output)
                .map_err(Clone::clone),
            "inbox",
        )?;
        result
    }
    fn interrupt(&self) -> bool {
        self.inner.interrupt()
    }
    async fn close(&self, token: CancellationToken) -> Result<(), SubagentError> {
        self.record
            .closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.record
            .update(|row| row.state = crate::TaskState::Cancelling)
            .map_err(task_io_error)?;
        self.record
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
        let _ = self.inner.interrupt();
        let _turn = self.record.turns.lock().await;
        let result = self.inner.close(token).await;
        self.record
            .update(|row| {
                row.state = if result.is_ok() {
                    crate::TaskState::Completed
                } else {
                    crate::TaskState::Failed
                }
            })
            .map_err(task_io_error)?;
        result
    }
}

impl SubagentRegistry {
    /// Attach bounded task history in the owning durable session directory.
    /// Active and retained handles from a previous process become Interrupted.
    pub fn attach_task_history(&self, root: std::path::PathBuf) -> Result<(), SubagentError> {
        std::fs::create_dir_all(&root).map_err(task_io_error)?;
        self.budget
            .attach_history(root.with_file_name("subagent-budget.json"))
            .map_err(task_io_error)?;
        let mut slot = self
            .history_root
            .lock()
            .map_err(|_| task_io_error(std::io::Error::other("history lock poisoned")))?;
        if slot.is_some() {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "task history already attached",
            ));
        }
        let mut recovered = Vec::new();
        for entry in std::fs::read_dir(&root).map_err(task_io_error)? {
            let path = entry.map_err(task_io_error)?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(task_io_error)?;
            if !metadata.is_file() || metadata.len() > 64 * 1024 {
                return Err(task_io_error(std::io::Error::other(
                    "unsafe task history file",
                )));
            }
            let snapshot: crate::TaskSnapshot =
                serde_json::from_slice(&std::fs::read(&path).map_err(task_io_error)?)
                    .map_err(|e| task_io_error(e.into()))?;
            let id = SubagentId::new(&snapshot.id)
                .map_err(|e| SubagentError::new(SubagentErrorCode::Failed, e.to_string()))?;
            let record = Arc::new(crate::task_inventory::TaskRecord::new(
                snapshot,
                CancellationToken::new(),
                Some(path),
            ));
            record.update(|row| {
                if row.state.active() || row.state == crate::TaskState::Idle || row.state == crate::TaskState::Closed {
                    row.state = crate::TaskState::Interrupted;
                    row.output = "Process ended; execution did not survive. Start a new task explicitly.".to_owned();
                }
            }).map_err(task_io_error)?;
            recovered.push((id, record));
        }
        recovered.sort_by_key(|(_, record)| record.read().created_at_ms);
        while recovered.len() > 256 {
            let (_, record) = recovered.remove(0);
            record.remove_file();
        }
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(recovered);
        *slot = Some(root);
        Ok(())
    }

    fn admit_task(
        &self,
        request: &mut SubagentRequest,
        cancellation: CancellationToken,
    ) -> Result<SubagentId, SubagentError> {
        let workspace = self.workspace_paused.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "workspace admission unavailable")
        })?;
        if *workspace
            || self
                .workspace_closed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "workspace transition is active",
            ));
        }
        if !self.recognizes_authority(request.authority()) {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "invalid task authority",
            ));
        }
        let parent = self.agent_for_authority(request.authority());
        let provider = self.select(request).ok_or_else(|| {
            SubagentError::new(
                SubagentErrorCode::Unsupported,
                "no provider supports requested task mode",
            )
        })?;
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        // Bound settled history without imposing a count limit on live conversations.
        while tasks.len() >= 256 {
            let oldest = tasks
                .iter()
                .filter(|(_, r)| {
                    !r.read().state.active() && r.read().state != crate::TaskState::Idle
                })
                .min_by_key(|(_, r)| r.read().created_at_ms)
                .map(|(id, _)| id.clone());
            let Some(id) = oldest else {
                break;
            };
            if let Some(record) = tasks.remove(&id) {
                self.archived
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                record.remove_file();
            }
        }
        let sequence = self
            .next_task
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = SubagentId::new(format!(
            "task-{}-{}-{sequence}",
            crate::task_inventory::now_ms(),
            std::process::id()
        ))
        .map_err(|e| SubagentError::new(SubagentErrorCode::Failed, e.to_string()))?;
        let root = self
            .history_root
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let now = crate::task_inventory::now_ms();
        let record = Arc::new(
            crate::task_inventory::TaskRecord::new(
                crate::TaskSnapshot {
                    id: id.to_string(),
                    owner: request.authority().owner().to_string(),
                    label: request.label().to_owned(),
                    provider: provider.descriptor().id().to_string(),
                    session_id: None,
                    workspace: None,
                    job_id: None,
                    spawn_call_id: crate::code_mode::ORIGIN_TOOL_CALL
                        .try_with(Clone::clone)
                        .ok(),
                    usage: heycode_session::WorkflowUsage::default(),
                    state: crate::TaskState::Queued,
                    revision: 0,
                    created_at_ms: now,
                    updated_at_ms: now,
                    output: String::new(),
                    output_truncated: false,
                    terminal_diagnostic: None,
                    diagnostics: Vec::new(),
                },
                cancellation,
                root.map(|root| root.join(format!("{id}.json"))),
            )
            .with_observer(request.observer.clone()),
        );
        *record.parent.lock().unwrap_or_else(|e| e.into_inner()) = parent;
        record.update(|_| {}).map_err(task_io_error)?;
        tasks.insert(id.clone(), record.clone());
        request.task = Some(record);
        Ok(id)
    }

    fn task_record_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Option<Arc<crate::task_inventory::TaskRecord>> {
        if !self.recognizes_authority(authority) {
            return None;
        }
        self.tasks
            .lock()
            .ok()?
            .get(id)
            .filter(|r| r.read().owner == authority.owner().as_str())
            .cloned()
    }

    /// Acknowledge an exact owned diagnostic without changing execution or evidence.
    pub fn acknowledge_task_diagnostic_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
        diagnostic_id: &str,
    ) -> Result<(), SubagentError> {
        let record = self
            .task_record_for(authority, id)
            .ok_or_else(|| SubagentError::new(SubagentErrorCode::Refused, "unknown task"))?;
        if !record
            .read()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.id == diagnostic_id)
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "diagnostic is no longer retained",
            ));
        }
        record
            .update(|row| {
                for diagnostic in row
                    .diagnostics
                    .iter_mut()
                    .chain(row.terminal_diagnostic.iter_mut())
                {
                    if diagnostic.id == diagnostic_id {
                        diagnostic.acknowledged = true;
                    }
                }
            })
            .map_err(task_io_error)
    }

    /// All task records for the human task console. Model callers use the owner-filtered method.
    #[must_use]
    pub fn task_snapshots(&self) -> Vec<crate::TaskSnapshot> {
        self.tasks
            .lock()
            .map(|rows| rows.values().map(|row| row.read()).collect())
            .unwrap_or_default()
    }

    /// All admitted tasks of this owner, including one-shot and terminal history.
    #[must_use]
    pub fn task_snapshots_for(&self, authority: &SubagentAuthority) -> Vec<crate::TaskSnapshot> {
        if !self.recognizes_authority(authority) {
            return Vec::new();
        }
        self.task_snapshots()
            .into_iter()
            .filter(|row| row.owner == authority.owner().as_str())
            .collect()
    }

    /// Live native child, available before first inference for transcript/events/control.
    #[must_use]
    pub fn native_child_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Option<Arc<Agent>> {
        self.task_record_for(authority, id)?
            .native
            .lock()
            .ok()?
            .upgrade()
    }

    /// Bounded wait for a task revision; timeout returns the unchanged snapshot.
    pub async fn wait_task_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
        after_revision: u64,
        timeout: std::time::Duration,
        cancellation: CancellationToken,
    ) -> Result<crate::TaskSnapshot, SubagentError> {
        let record = self
            .task_record_for(authority, id)
            .ok_or_else(|| SubagentError::new(SubagentErrorCode::Refused, "unknown task"))?;
        let deadline =
            tokio::time::Instant::now() + timeout.min(std::time::Duration::from_secs(60));
        loop {
            let changed = record.changed.notified();
            let snapshot = record.read();
            if snapshot.revision > after_revision {
                return Ok(snapshot);
            }
            tokio::select! {
                () = changed => {},
                () = tokio::time::sleep_until(deadline) => return Ok(record.read()),
                () = cancellation.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "task wait cancelled")),
            }
        }
    }

    /// Wait until any owner-scoped agent advances, then return every target in
    /// input order. The wait owns no child cancellation token or execution task.
    pub async fn wait_tasks_for(
        &self,
        authority: &SubagentAuthority,
        targets: &[(SubagentId, u64)],
        timeout: std::time::Duration,
        cancellation: CancellationToken,
    ) -> Result<Vec<crate::TaskSnapshot>, SubagentError> {
        use futures::StreamExt;
        if !(1..=8).contains(&targets.len()) {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "wait requires 1..=8 agents",
            ));
        }
        // Resolve every authority before waiting; one inaccessible target must
        // not allow partial success or disclose a different owner's snapshot.
        let records = targets
            .iter()
            .map(|(id, _)| {
                self.task_record_for(authority, id)
                    .ok_or_else(|| SubagentError::new(SubagentErrorCode::Refused, "unknown agent"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let deadline =
            tokio::time::Instant::now() + timeout.min(std::time::Duration::from_secs(60));
        loop {
            let mut changes = futures::stream::FuturesUnordered::new();
            for record in &records {
                // Register before the snapshot read, closing the lost-wakeup gap.
                let mut notification = Box::pin(record.changed.notified());
                notification.as_mut().enable();
                changes.push(notification);
            }
            let snapshots: Vec<_> = records.iter().map(|record| record.read()).collect();
            if snapshots
                .iter()
                .zip(targets)
                .any(|(snapshot, (_, revision))| snapshot.revision > *revision)
            {
                return Ok(snapshots);
            }
            tokio::select! {
                _ = changes.next() => {},
                () = tokio::time::sleep_until(deadline) => return Ok(records.iter().map(|record| record.read()).collect()),
                () = cancellation.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "agent wait cancelled")),
            }
        }
    }

    /// Archive a settled continuable child without destroying its native session.
    pub fn archive_child_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Result<bool, SubagentError> {
        let Some(record) = self.task_record_for(authority, id) else {
            return Ok(false);
        };
        let _turn = record.turns.try_lock().map_err(|_| {
            SubagentError::new(
                SubagentErrorCode::Refused,
                "interrupt and wait before closing a running child",
            )
        })?;
        if record.read().state.active() {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "interrupt and wait before closing a running child",
            ));
        }
        let entry = self
            .children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(entry) = entry {
            self.archived
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.clone(), entry);
            record
                .update(|row| row.state = crate::TaskState::Closed)
                .map_err(task_io_error)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Restore an archived conversation in the current process; restart never fabricates a handle.
    pub fn restore_child_for(
        &self,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Result<bool, SubagentError> {
        let workspace = self.workspace_paused.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "workspace admission unavailable")
        })?;
        if *workspace
            || self
                .workspace_closed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "workspace transition is active",
            ));
        }
        let Some(record) = self.task_record_for(authority, id) else {
            return Ok(false);
        };
        let entry = self
            .archived
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(entry) = entry {
            self.children
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.clone(), entry);
            record
                .update(|row| row.state = crate::TaskState::Idle)
                .map_err(task_io_error)?;
            if let Some(native) = record
                .native
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade()
            {
                native
                    .inbox_auto_paused
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Native continuable parents own automatic completion turns even with no UI attached.
    /// Root conversation wakes remain the root host's responsibility.
    pub(crate) fn wake_native_parent(
        self: &Arc<Self>,
        id: &SubagentId,
        jobs: &Arc<JobRegistry>,
    ) -> Result<(), SubagentError> {
        use std::sync::atomic::Ordering;
        let record = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned();
        let agent = if let Some(record) = &record {
            if record.read().provider != "native"
                || matches!(
                    record.read().state,
                    crate::TaskState::Closed
                        | crate::TaskState::Cancelled
                        | crate::TaskState::Interrupted
                )
                || record.closing.load(Ordering::SeqCst)
            {
                return Ok(());
            }
            record
                .native
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .upgrade()
        } else {
            self.parent_agent().filter(|agent| {
                agent
                    .session()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .id()
                    .as_str()
                    == id.as_str()
            })
        };
        let Some(agent) = agent else {
            return Ok(());
        };
        if agent.inbox_auto_paused.load(Ordering::SeqCst)
            || agent.token().is_cancelled()
            || agent.inbox_wake_deferred.load(Ordering::SeqCst) > 0
        {
            return Ok(());
        }
        if agent.next_wakeable_message().is_none()
            || agent.inbox_driver_running.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        let registry = self.clone();
        let parent_id = id.clone();
        let owned_jobs = jobs.clone();
        let recipient = agent.clone();
        struct DriverReservation(Arc<Agent>);
        impl Drop for DriverReservation {
            fn drop(&mut self) {
                self.0.inbox_driver_running.store(false, Ordering::SeqCst);
            }
        }
        let driver = DriverReservation(recipient.clone());
        let spawned = jobs.spawn_coordinator(format!("resume {id}"), InboxDelivery::Inject, move |job, cancellation| async move {
            let result = async {
                if let Some(record) = record {
                    loop {
                        let changed = record.changed.notified();
                        tokio::pin!(changed);
                        changed.as_mut().enable();
                        if !record.read().state.active() { break; }
                        tokio::select! {
                            () = changed => {},
                            () = cancellation.cancelled() => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "native parent wake cancelled")),
                        }
                    }
                    if record.closing.load(Ordering::SeqCst) || matches!(record.read().state, crate::TaskState::Closed | crate::TaskState::Cancelled | crate::TaskState::Interrupted) || recipient.next_wakeable_message().is_none() { return Ok(()); }
                    let handle = registry.children.lock().unwrap_or_else(|e| e.into_inner()).get(&parent_id).map(|entry| entry.handle.clone());
                    if let Some(handle) = handle { handle.run_mail(cancellation.clone()).await?; }
                    else { return Err(SubagentError::new(SubagentErrorCode::Unsupported, "recipient has no retained inbox runtime")); }
                } else {
                    for _ in 0..64 {
                        let Some(id) = recipient.next_wakeable_message() else { return Ok(()); };
                        match crate::subagent::with_task_context(recipient.clone(), registry.root_authority(parent_id.clone()), recipient.send_automatic_inbox_id_cancellable(&id, cancellation.clone())).await {
                            Ok(report) if report.reason == "aborted" => return Err(SubagentError::new(SubagentErrorCode::Cancelled, "root inbox turn cancelled")),
                            Ok(_) => {},
                            Err(error) if error.downcast_ref::<crate::FollowUpError>().is_some() => {},
                            Err(error) => return Err(SubagentError::new(SubagentErrorCode::Failed, error.to_string())),
                        }
                    }
                    if recipient.next_wakeable_message().is_some() { return Err(SubagentError::new(SubagentErrorCode::Refused, "root inbox turn budget exhausted")); }
                }
                Ok(())
            }.await;
            let outcome = if cancellation.is_cancelled() { JobOutcome::Cancelled } else if result.is_ok() { JobOutcome::Completed } else { JobOutcome::Failed };
            let _ = owned_jobs.settle_silent(&job, outcome);
            drop(driver);
            // Recheck after releasing ownership: an arrival before release was
            // coalesced; an arrival afterward can reserve the next driver.
            if result.is_ok() && !cancellation.is_cancelled() {
                let _ = registry.wake_native_parent(&parent_id, &owned_jobs);
            } else if let Err(error) = result {
                recipient.ui().emit(crate::UiEvent::Error { message: error.to_string() });
            }
        });
        if let Err(error) = spawned {
            agent.inbox_driver_running.store(false, Ordering::SeqCst);
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                format!("native inbox remains queued: {error}"),
            ));
        }
        Ok(())
    }

    /// Authorized message recipients. This is independent of control authority.
    fn message_targets(&self, authority: &SubagentAuthority) -> Vec<(String, String)> {
        if !self.recognizes_authority(authority) {
            return Vec::new();
        }
        let snapshots = self.task_snapshots();
        let caller = snapshots
            .iter()
            .find(|row| row.id == authority.owner().as_str());
        let mut ids = std::collections::BTreeSet::new();
        if let Some(caller) = caller {
            ids.insert(caller.owner.clone());
        }
        for row in &snapshots {
            if row.owner == authority.owner().as_str()
                || caller.is_some_and(|caller| row.owner == caller.owner && row.id != caller.id)
            {
                ids.insert(row.id.clone());
            }
        }
        // Derive the root from the recorded ancestry, never from a global host fallback.
        let mut root = authority.owner().as_str().to_owned();
        for _ in 0..=snapshots.len() {
            let Some(row) = snapshots.iter().find(|row| row.id == root) else {
                break;
            };
            root = row.owner.clone();
        }
        if root != authority.owner().as_str() {
            ids.insert(root);
        }
        ids.into_iter()
            .map(|id| {
                let name = snapshots
                    .iter()
                    .find(|row| row.id == id)
                    .map_or_else(|| "main".to_owned(), |row| row.label.clone());
                (id, name)
            })
            .collect()
    }

    pub(crate) fn message_roster(&self, authority: &SubagentAuthority) -> String {
        serde_json::to_string(
            &self
                .message_targets(authority)
                .into_iter()
                .take(64)
                .map(|(id, name)| serde_json::json!({"id":id,"name":name}))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_owned())
    }

    fn resolve_message_target(
        &self,
        authority: &SubagentAuthority,
        to: &str,
    ) -> Result<SubagentId, SubagentError> {
        let targets = self.message_targets(authority);
        let requested = if to == "parent" {
            self.task_snapshots()
                .into_iter()
                .find(|row| row.id == authority.owner().as_str())
                .map(|row| row.owner)
                .ok_or_else(|| {
                    SubagentError::new(SubagentErrorCode::Refused, "main has no parent")
                })?
        } else {
            to.to_owned()
        };
        if let Some((id, _)) = targets.iter().find(|(id, _)| id == &requested) {
            return SubagentId::new(id)
                .map_err(|e| SubagentError::new(SubagentErrorCode::Refused, e.to_string()));
        }
        let matches = targets
            .iter()
            .filter(|(_, name)| name == &requested)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "unknown or ambiguous agent recipient; use an exact roster ID",
            ));
        }
        let id = &matches[0].0;
        let mut aliases = self.message_aliases.lock().map_err(|_| {
            SubagentError::new(SubagentErrorCode::Failed, "message aliases unavailable")
        })?;
        let entry = aliases
            .entry((authority.owner().to_string(), requested))
            .or_insert_with(|| id.clone());
        if entry != id {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "agent name now refers to a different conversation; use its exact ID",
            ));
        }
        SubagentId::new(id)
            .map_err(|e| SubagentError::new(SubagentErrorCode::Refused, e.to_string()))
    }

    /// Admit a registry-attributed message without granting the sender control over its recipient.
    pub(crate) fn send_agent_message(
        self: &Arc<Self>,
        authority: &SubagentAuthority,
        to: &str,
        text: &str,
    ) -> Result<serde_json::Value, SubagentError> {
        if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "message is blank or too large",
            ));
        }
        let target = self.resolve_message_target(authority, to)?;
        let snapshots = self.task_snapshots();
        let sender = snapshots
            .iter()
            .find(|row| row.id == authority.owner().as_str());
        let sender_name = sender.map_or("main", |row| row.label.as_str());
        let run_id = sender.and_then(|row| row.job_id.as_deref()).map_or_else(
            || {
                self.agent_for_authority(authority)
                    .and_then(|agent| {
                        let session = agent.session().lock().ok()?;
                        session
                            .events()
                            .iter()
                            .rev()
                            .find_map(|event| match &event.kind {
                                heycode_session::SessionEventKind::TurnStart { turn } => {
                                    Some(format!("agent-run:{}:turn:{turn}", authority.owner()))
                                }
                                _ => None,
                            })
                    })
                    .unwrap_or_else(|| format!("agent-run:{}:unstarted", authority.owner()))
            },
            |job| format!("agent-run:{}:{job}", authority.owner()),
        );
        let source = heycode_session::InboxSource::Agent {
            agent_id: authority.owner().to_string(),
            agent_name: sender_name.to_owned(),
            recipient_id: target.to_string(),
            run_id,
            completion_id: None,
            outcome: None,
        };
        let envelope = format!(
            "[Agent message from {} ({})]\n{}",
            serde_json::to_string(sender_name).unwrap_or_default(),
            authority.owner(),
            text
        );
        if let Some(row) = snapshots.iter().find(|row| row.id == target.as_str()) {
            // Only this internal route uses recipient-owner authority. It is never returned to the model.
            let owner = SubagentId::new(&row.owner)
                .map_err(|e| SubagentError::new(SubagentErrorCode::Refused, e.to_string()))?;
            let route = SubagentAuthority::bound(self.authority_token.clone(), owner);
            let receipt = self.queue_message_from(
                &route,
                &target,
                envelope,
                true,
                InboxDelivery::Steer,
                Some((authority.clone(), source)),
            )?;
            return Ok(
                serde_json::json!({"agent_id":target.as_str(),"name":row.label,"message_id":receipt.message_id.map(|id|id.to_string()),"status":"queued"}),
            );
        }
        let root_authority = SubagentAuthority::bound(self.authority_token.clone(), target.clone());
        let recipient = self.agent_for_authority(&root_authority).ok_or_else(|| {
            SubagentError::new(
                SubagentErrorCode::Unsupported,
                "recipient runtime is unavailable",
            )
        })?;
        if recipient.token().is_cancelled() {
            return Err(SubagentError::new(
                SubagentErrorCode::Cancelled,
                "recipient was stopped; user resume is required",
            ));
        }
        let (id, wake) = recipient
            .submit_inbox_with_source(InboxDelivery::Steer, envelope, source)
            .map_err(|e| SubagentError::new(SubagentErrorCode::Failed, e.to_string()))?;
        if matches!(wake, crate::InboxWake::Wake) {
            let host = self
                .job_host
                .lock()
                .ok()
                .and_then(|host| host.clone())
                .ok_or_else(|| {
                    SubagentError::new(
                        SubagentErrorCode::Unsupported,
                        "message dispatcher unavailable",
                    )
                })?;
            self.wake_native_parent(&target, &host.jobs)?;
        }
        Ok(
            serde_json::json!({"agent_id":target.as_str(),"name":"main","message_id":id.as_str(),"status":"queued"}),
        )
    }

    /// Whether an owned failed run still has its native conversation/configuration.
    /// Startup failures, one-shot runs and recovered history cannot be replayed
    /// from their diagnostic text alone.
    #[must_use]
    pub fn retry_available_for(&self, authority: &SubagentAuthority, id: &SubagentId) -> bool {
        let Some(record) = self.task_record_for(authority, id) else {
            return false;
        };
        let snapshot = record.read();
        snapshot.provider == "native"
            && snapshot.state == crate::TaskState::Failed
            && !record.closing.load(std::sync::atomic::Ordering::SeqCst)
            && record
                .native
                .lock()
                .is_ok_and(|native| native.strong_count() > 0)
            && self
                .children
                .lock()
                .is_ok_and(|children| children.contains_key(id))
    }

    /// Explicitly retry an owned failed native run in its retained conversation.
    /// This is a new invocation: it never recreates a lost runtime or resumes a
    /// user-cancelled task. The child reconciles partial effects before retrying.
    pub fn retry_for(
        self: &Arc<Self>,
        authority: &SubagentAuthority,
        id: &SubagentId,
    ) -> Result<SubagentMessageReceipt, SubagentError> {
        let record = self.task_record_for(authority, id).ok_or_else(|| {
            SubagentError::new(SubagentErrorCode::Refused, "unknown or unauthorized task")
        })?;
        if record.read().state != crate::TaskState::Failed {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "only a failed run can be explicitly retried; stopped tasks require an explicit resume",
            ));
        }
        if !self.retry_available_for(authority, id) {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "retry unavailable: this failed run has no retained native conversation and configuration; create a new task with an explicit prompt",
            ));
        }
        self.queue_message(authority, id,
            "The user explicitly requested a retry of this failed run. Continue in the retained conversation. Inspect the recorded failure and reconcile any partially completed effects before retrying; preserve completed work and report the resulting outcome.".to_owned(),
            false, InboxDelivery::Steer)
    }

    /// Compatibility helper returning the owned execution job for a queued message.
    pub fn send_background(
        self: &Arc<Self>,
        authority: &SubagentAuthority,
        id: &SubagentId,
        text: String,
        steer: bool,
        delivery: InboxDelivery,
    ) -> Result<JobId, SubagentError> {
        self.queue_message(authority, id, text, steer, delivery)
            .map(|receipt| receipt.job_id)
    }

    /// Reserve an owned job and durably queue native follow-up/steer input before returning.
    /// Active native turns consume steer at a step boundary. The owned driver handles idle races.
    pub fn queue_message(
        self: &Arc<Self>,
        authority: &SubagentAuthority,
        id: &SubagentId,
        text: String,
        steer: bool,
        delivery: InboxDelivery,
    ) -> Result<SubagentMessageReceipt, SubagentError> {
        self.queue_message_from(authority, id, text, steer, delivery, None)
    }

    fn queue_message_from(
        self: &Arc<Self>,
        authority: &SubagentAuthority,
        id: &SubagentId,
        text: String,
        steer: bool,
        delivery: InboxDelivery,
        origin: Option<(SubagentAuthority, heycode_session::InboxSource)>,
    ) -> Result<SubagentMessageReceipt, SubagentError> {
        if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "message is blank or too large",
            ));
        }
        let record = self
            .task_record_for(authority, id)
            .ok_or_else(|| SubagentError::new(SubagentErrorCode::Refused, "unknown task"))?;
        let snapshot = record.read();
        if matches!(
            snapshot.state,
            crate::TaskState::Closed | crate::TaskState::Interrupted | crate::TaskState::Cancelled
        ) {
            return Err(SubagentError::new(
                SubagentErrorCode::Refused,
                "restore or restart the task before sending",
            ));
        }
        if steer && snapshot.provider != "native" {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "this provider does not support native inbox steering; use a background follow-up",
            ));
        }
        if !snapshot.state.active() && self.child_for(authority, id).is_none() {
            return Err(SubagentError::new(
                SubagentErrorCode::Unsupported,
                "agent is one-shot or its runtime is unavailable; start a new agent explicitly",
            ));
        }
        let host = self
            .job_host
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                SubagentError::new(
                    SubagentErrorCode::Unsupported,
                    "background host unavailable",
                )
            })?;
        let native = self.native_child_for(authority, id);
        let queued_native = native.clone();
        let registry = self.clone();
        let authority = authority.clone();
        let id = id.clone();
        let jobs = host.jobs.clone();
        let reply_authority = origin
            .as_ref()
            .map_or_else(|| authority.clone(), |(sender, _)| sender.clone());
        let parent = self.agent_for_authority(&reply_authority);
        let source = origin.map(|(_, source)| source);
        let delayed_source = source.clone();
        let native_recipient = snapshot.provider == "native";
        let reply_owner = reply_authority.owner().clone();
        let sender_name = snapshot.label.clone();
        // Reservation precedes inbox mutation. The job cannot dispatch until durable append succeeds.
        let (admit, ready) =
            tokio::sync::oneshot::channel::<Option<heycode_session::InboxMessageId>>();
        let message = text.clone();
        let job = host.jobs.spawn_coordinator(format!("message to {id}"), delivery, move |job, cancellation| async move {
            let result = async {
                let mut queued_native = queued_native;
                let mut pending = ready.await.map_err(|_| SubagentError::new(SubagentErrorCode::Cancelled, "message admission failed"))?;
                loop {
                    let changed = record.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if !record.read().state.active() { break; }
                    tokio::select! {
                        () = changed => {},
                        () = cancellation.cancelled() => {
                            if let (Some(native), Some(pending)) = (&queued_native, &pending) { let _ = native.cancel_inbox(pending); }
                            return Err(SubagentError::new(SubagentErrorCode::Cancelled, "message cancelled"));
                        }
                    }
                }
                if cancellation.is_cancelled() {
                    if let (Some(native), Some(pending)) = (&queued_native, &pending) { let _ = native.cancel_inbox(pending); }
                    return Err(SubagentError::new(SubagentErrorCode::Cancelled, "message cancelled"));
                }
                if pending.is_none() && native_recipient {
                    let native = registry.native_child_for(&authority, &id).ok_or_else(|| SubagentError::new(SubagentErrorCode::Unsupported, "recipient startup did not retain a native inbox"))?;
                    let (message_id, _) = native.submit_inbox_with_source(
                        if steer { InboxDelivery::Steer } else { InboxDelivery::FollowUp },
                        message.clone(), delayed_source.clone().unwrap_or(heycode_session::InboxSource::Human),
                    ).map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))?;
                    pending = Some(message_id);
                    queued_native = Some(native);
                }
                if pending.is_none() && delayed_source.is_some() {
                    return Err(SubagentError::new(SubagentErrorCode::Unsupported, "recipient provider cannot preserve agent message provenance"));
                }
                if let (Some(native), Some(pending)) = (&queued_native, &pending) {
                    let session = native.session().lock().unwrap_or_else(|e| e.into_inner());
                    let inbox = session.inbox();
                    if !inbox.next_turn().iter().chain(inbox.next_step()).any(|message| message.id() == pending) {
                        return Ok(None);
                    }
                }
                if matches!(record.read().state, crate::TaskState::Cancelled | crate::TaskState::Interrupted | crate::TaskState::Closed) {
                    return Err(SubagentError::new(SubagentErrorCode::Cancelled, "recipient stopped before message dispatch; user resume is required"));
                }
                let child = registry.child_for(&authority, &id)
                    .ok_or_else(|| SubagentError::new(SubagentErrorCode::Refused, "task has no continuable handle"))?;
                let result = if let Some(pending) = &pending {
                    child.run_pending_in_job(pending, &job, cancellation.clone()).await
                } else {
                    child.send_in_job(&message, &job, cancellation.clone()).await
                };
                if cancellation.is_cancelled()
                    && let (Some(native), Some(pending)) = (&queued_native, &pending)
                {
                    let _ = native.cancel_inbox(pending);
                }
                result.map(Some)
            }.await;
            let (outcome, notice) = match result {
                Ok(None) => { let _ = jobs.settle_silent(&job, JobOutcome::Completed); return; }
                Err(error) if error.code() == SubagentErrorCode::AlreadyConsumed => { let _ = jobs.settle_silent(&job, JobOutcome::Completed); return; }
                Ok(Some(text)) => (JobOutcome::Completed, bounded_job_notice(&text)),
                Err(error) => (if cancellation.is_cancelled() { JobOutcome::Cancelled } else { JobOutcome::Failed }, bounded_job_notice(error.message())),
            };
            if let Some(parent) = parent {
                let settlement = JobSettlement::bounded_or_failed(outcome, notice);
                let source = parent.agent_completion_source(&job, id.as_str(), &sender_name, reply_owner.as_str(), settlement.outcome());
                match parent.settle_agent_job_with_retry(&jobs, &job, &settlement, source, cancellation.clone()).await {
                    Ok(crate::WakeDecision::Woke) => if let Err(error) = registry.wake_native_parent(&reply_owner, &jobs) { parent.ui().emit(crate::UiEvent::Error { message: error.to_string() }); },
                    Ok(_) => {},
                    Err(error) => parent.ui().emit(crate::UiEvent::Error { message: format!("agent result remains pending delivery: {error}") }),
                }
            }
        }).map_err(|e| SubagentError::new(SubagentErrorCode::Refused, e.to_string()))?;
        let message_id = if let Some(native) = native {
            match native.submit_inbox_with_source(
                if steer {
                    InboxDelivery::Steer
                } else {
                    InboxDelivery::FollowUp
                },
                text,
                source.unwrap_or(heycode_session::InboxSource::Human),
            ) {
                Ok((id, _)) => Some(id),
                Err(error) => {
                    let _ = host.jobs.cancel(&job);
                    return Err(SubagentError::new(
                        SubagentErrorCode::Failed,
                        error.to_string(),
                    ));
                }
            }
        } else {
            None
        };
        let _ = admit.send(message_id.clone());
        Ok(SubagentMessageReceipt {
            job_id: job,
            message_id,
        })
    }
}

impl SubagentRegistry {
    pub(crate) fn parent_agent(&self) -> Option<Arc<Agent>> {
        self.job_host.lock().ok()?.as_ref()?.agent.upgrade()
    }
}

/// Observer invoked synchronously after child registration and before its first provider call.
type NativeChildObservers = std::sync::Mutex<Vec<(Arc<()>, NativeChildObserver)>>;
/// Callback invoked before native inference; observers receive the actual child Agent.
pub type NativeChildObserver = Arc<dyn Fn(&SubagentId, Arc<Agent>) + Send + Sync>;
/// Exact removal guard for a native-child observer.
pub struct NativeChildObservation {
    observers: std::sync::Weak<NativeChildObservers>,
    token: Arc<()>,
}
impl Drop for NativeChildObservation {
    fn drop(&mut self) {
        if let Some(observers) = self.observers.upgrade()
            && let Ok(mut rows) = observers.lock()
        {
            rows.retain(|(token, _)| !Arc::ptr_eq(token, &self.token));
        }
    }
}
impl SubagentRegistry {
    /// Subscribe before starting children. Keep the returned guard for the UI lifetime.
    pub fn attach_native_child_observer(
        &self,
        observer: NativeChildObserver,
    ) -> NativeChildObservation {
        let token = Arc::new(());
        self.native_observers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((token.clone(), observer));
        NativeChildObservation {
            observers: Arc::downgrade(&self.native_observers),
            token,
        }
    }
    pub(crate) fn publish_native_child(&self, id: &SubagentId, agent: &Arc<Agent>) {
        let observers = self
            .native_observers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(_, observer)| observer.clone())
            .collect::<Vec<_>>();
        for observer in observers {
            observer(id, agent.clone());
        }
    }
}

impl SubagentRegistry {
    /// Empty registry with explicit shared inference/spend guardrails.
    #[must_use]
    pub fn with_budget(limits: crate::SubagentBudgetLimits) -> Self {
        Self {
            budget: Arc::new(crate::subagent_budget::SubagentBudget::new(limits)),
            ..Self::default()
        }
    }
    /// Remaining aggregate provider admission budget for all native descendants.
    #[must_use]
    pub fn budget_snapshot(&self) -> crate::SubagentBudgetSnapshot {
        self.budget.snapshot()
    }
}

/// Admission receipt for a nonblocking child message.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubagentMessageReceipt {
    /// Owned execution job, also usable when a provider lacks a durable inbox.
    pub job_id: JobId,
    /// Native durable inbox identity; absent until a just-admitted child has a session.
    pub message_id: Option<heycode_session::InboxMessageId>,
}

/// Current provider readiness, separate from its static capability descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentReadiness {
    /// Native execution or an authenticated external runtime is ready.
    Ready,
    /// Login must be completed with the provider's own setup flow.
    NeedsAuthentication,
    /// Executable/runtime is unavailable.
    Unavailable,
    /// Provider cannot prove readiness; start may still fail during its handshake.
    Unknown,
}
impl SubagentRegistry {
    /// Probe one exact provider without pretending the catalog is a readiness check.
    pub async fn provider_readiness(
        &self,
        id: &SubagentProviderId,
        cancellation: CancellationToken,
    ) -> Result<SubagentReadiness, SubagentError> {
        let provider = self
            .providers
            .lock()
            .map_err(|_| SubagentError::new(SubagentErrorCode::Failed, "registry unavailable"))?
            .iter()
            .find(|entry| entry.provider.descriptor().id() == id)
            .map(|entry| entry.provider.clone())
            .ok_or_else(|| {
                SubagentError::new(SubagentErrorCode::Unsupported, "provider is not registered")
            })?;
        provider.readiness(cancellation).await
    }
}

struct TaskExecutionLease {
    record: Arc<crate::task_inventory::TaskRecord>,
    settled: bool,
}
impl Drop for TaskExecutionLease {
    fn drop(&mut self) {
        if !self.settled {
            self.record
                .cancellation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel();
            let _ = self.record.finish(
                crate::TaskState::Interrupted,
                "Task owner ended before execution settled; no automatic retry.",
            );
        }
    }
}

impl SubagentRegistry {
    pub(crate) fn agent_for_authority(&self, authority: &SubagentAuthority) -> Option<Arc<Agent>> {
        if !self.recognizes_authority(authority) {
            return None;
        }
        if let Some(record) = self.tasks.lock().ok()?.get(authority.owner()).cloned() {
            return record.native.lock().ok()?.upgrade();
        }
        let agent = self.parent_agent()?;
        let matches = agent.session().lock().ok()?.id().as_str() == authority.owner().as_str();
        matches.then_some(agent)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request(seed: SubagentSeed, continuation: SubagentContinuation) -> SubagentRequest {
        SubagentRequest::new("review", "check the diff", seed, continuation, 0).unwrap()
    }

    fn descriptor(capabilities: SubagentCapabilities) -> SubagentProviderDescriptor {
        SubagentProviderDescriptor::new("native", "Native child agent", capabilities).unwrap()
    }

    #[test]
    fn unknown_capability_never_counts_as_support() {
        let unknown = descriptor(SubagentCapabilities {
            fork: CapabilitySupport::Unknown,
            continuation: CapabilitySupport::Unknown,
            interrupt: CapabilitySupport::Unknown,
        });
        assert!(unknown.supports(&request(SubagentSeed::Fresh, SubagentContinuation::OneShot)));
        assert!(!unknown.supports(&request(
            SubagentSeed::ForkParent,
            SubagentContinuation::OneShot
        )));
        assert!(!unknown.supports(&request(
            SubagentSeed::Fresh,
            SubagentContinuation::Continuable
        )));
    }

    #[test]
    fn identifiers_and_requests_validate_at_the_boundary() {
        assert!(SubagentProviderId::new("native-worktree").is_ok());
        assert!(SubagentProviderId::new("Native").is_err());
        assert!(SubagentProviderId::new("").is_err());
        assert!(SubagentId::new("0199c3d0-1c3f").is_ok());
        assert!(SubagentId::new(" leading").is_err());
        assert!(SubagentId::new("has space").is_err());
        assert!(
            SubagentRequest::new(
                "l",
                "   ",
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
                0
            )
            .is_err()
        );
        assert!(
            SubagentRequest::new(
                "bad\nlabel",
                "p",
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn errors_stay_bounded_and_single_line() {
        let error = SubagentError::new(
            SubagentErrorCode::Failed,
            format!("a\nb{}", "x".repeat(900)),
        );
        assert_eq!(error.code(), SubagentErrorCode::Failed);
        assert!(error.message().len() <= 512);
        assert!(!error.message().contains('\n'));
    }

    struct OwnedHandle {
        id: SubagentId,
        label: String,
    }

    #[async_trait]
    impl SubagentHandle for OwnedHandle {
        fn id(&self) -> &SubagentId {
            &self.id
        }

        fn label(&self) -> &str {
            &self.label
        }

        async fn send(
            &self,
            _text: &str,
            _cancellation: CancellationToken,
        ) -> Result<String, SubagentError> {
            Ok("ok".to_owned())
        }

        fn interrupt(&self) -> bool {
            true
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), SubagentError> {
            Ok(())
        }
    }

    struct OwnedProvider {
        descriptor: SubagentProviderDescriptor,
        starts: AtomicUsize,
    }

    #[async_trait]
    impl SubagentProvider for OwnedProvider {
        fn descriptor(&self) -> &SubagentProviderDescriptor {
            &self.descriptor
        }

        async fn start(
            &self,
            request: SubagentRequest,
            _cancellation: CancellationToken,
        ) -> Result<SubagentStarted, SubagentError> {
            let index = self.starts.fetch_add(1, Ordering::SeqCst);
            let id = SubagentId::new(format!("child-{index}")).unwrap();
            Ok(SubagentStarted {
                id: id.clone(),
                text: "started".to_owned(),
                handle: (request.continuation == SubagentContinuation::Continuable).then(|| {
                    Arc::new(OwnedHandle {
                        id,
                        label: request.label().to_owned(),
                    }) as Arc<dyn SubagentHandle>
                }),
            })
        }
    }

    #[tokio::test]
    async fn registry_authority_hides_foreign_children_and_refuses_unbound_requests() {
        let registry = SubagentRegistry::new();
        registry
            .register(Arc::new(OwnedProvider {
                descriptor: descriptor(SubagentCapabilities {
                    fork: CapabilitySupport::Supported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                }),
                starts: AtomicUsize::new(0),
            }))
            .unwrap();
        let owner_a = registry.root_authority(SubagentId::new("owner-a").unwrap());
        let owner_b = registry.root_authority(SubagentId::new("owner-b").unwrap());

        let unbound = SubagentRequest::new(
            "unbound",
            "must fail",
            SubagentSeed::Fresh,
            SubagentContinuation::OneShot,
            0,
        )
        .unwrap();
        assert_eq!(
            registry
                .start(unbound, CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            SubagentErrorCode::Refused
        );

        for (label, authority) in [("a", owner_a.clone()), ("b", owner_b.clone())] {
            registry
                .start(
                    SubagentRequest::with_authority(
                        label,
                        "work",
                        SubagentSeed::Fresh,
                        SubagentContinuation::Continuable,
                        authority,
                    )
                    .unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
        }
        let child_a = registry.children_for(&owner_a)[0].0.clone();
        let child_b = registry.children_for(&owner_b)[0].0.clone();
        assert!(registry.child_for(&owner_a, &child_a).is_some());
        assert!(registry.child_for(&owner_b, &child_a).is_none());
        assert!(!registry.interrupt_for(&owner_b, &child_a));
        assert_eq!(registry.children_for(&owner_a), [(child_a, "a".to_owned())]);
        assert_eq!(registry.children_for(&owner_b), [(child_b, "b".to_owned())]);
    }

    #[derive(Clone, Copy)]
    enum ContractMode {
        ExtraHandle,
        MissingHandle,
        MismatchedId,
        DuplicateId,
    }

    struct ContractHandle {
        id: SubagentId,
        closed: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SubagentHandle for ContractHandle {
        fn id(&self) -> &SubagentId {
            &self.id
        }

        fn label(&self) -> &str {
            "contract"
        }

        async fn send(
            &self,
            _text: &str,
            _cancellation: CancellationToken,
        ) -> Result<String, SubagentError> {
            Ok(String::new())
        }

        fn interrupt(&self) -> bool {
            false
        }

        async fn close(&self, _cancellation: CancellationToken) -> Result<(), SubagentError> {
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct ContractProvider {
        descriptor: SubagentProviderDescriptor,
        mode: ContractMode,
        starts: AtomicUsize,
        closed: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SubagentProvider for ContractProvider {
        fn descriptor(&self) -> &SubagentProviderDescriptor {
            &self.descriptor
        }

        async fn start(
            &self,
            request: SubagentRequest,
            _cancellation: CancellationToken,
        ) -> Result<SubagentStarted, SubagentError> {
            let index = self.starts.fetch_add(1, Ordering::SeqCst);
            let id = SubagentId::new(match self.mode {
                ContractMode::DuplicateId => "duplicate".to_owned(),
                _ => format!("contract-{index}"),
            })
            .unwrap();
            let handle = match self.mode {
                ContractMode::MissingHandle => None,
                ContractMode::MismatchedId => Some(Arc::new(ContractHandle {
                    id: SubagentId::new("foreign").unwrap(),
                    closed: self.closed.clone(),
                }) as Arc<dyn SubagentHandle>),
                ContractMode::ExtraHandle | ContractMode::DuplicateId => {
                    Some(Arc::new(ContractHandle {
                        id: id.clone(),
                        closed: self.closed.clone(),
                    }) as Arc<dyn SubagentHandle>)
                }
            };
            let _ = request;
            Ok(SubagentStarted {
                id,
                text: "started".to_owned(),
                handle,
            })
        }
    }

    fn contract_registry(
        mode: ContractMode,
    ) -> (SubagentRegistry, SubagentAuthority, Arc<AtomicUsize>) {
        let registry = SubagentRegistry::new();
        let authority = registry.root_authority(SubagentId::new("owner").unwrap());
        let closed = Arc::new(AtomicUsize::new(0));
        registry
            .register(Arc::new(ContractProvider {
                descriptor: descriptor(SubagentCapabilities {
                    fork: CapabilitySupport::Supported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                }),
                mode,
                starts: AtomicUsize::new(0),
                closed: closed.clone(),
            }))
            .unwrap();
        (registry, authority, closed)
    }

    fn authorized_request(
        continuation: SubagentContinuation,
        authority: SubagentAuthority,
    ) -> SubagentRequest {
        SubagentRequest::with_authority(
            "contract",
            "work",
            SubagentSeed::Fresh,
            continuation,
            authority,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn provider_handle_shape_identity_and_uniqueness_are_admitted_before_publication() {
        for (mode, continuation, expected_closes) in [
            (ContractMode::ExtraHandle, SubagentContinuation::OneShot, 1),
            (
                ContractMode::MissingHandle,
                SubagentContinuation::Continuable,
                0,
            ),
            (
                ContractMode::MismatchedId,
                SubagentContinuation::Continuable,
                1,
            ),
        ] {
            let (registry, authority, closed) = contract_registry(mode);
            let error = registry
                .start(
                    authorized_request(continuation, authority),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), SubagentErrorCode::Refused);
            assert_eq!(registry.live_child_count(), 0);
            assert_eq!(closed.load(Ordering::SeqCst), expected_closes);
        }

        let (registry, authority, closed) = contract_registry(ContractMode::DuplicateId);
        registry
            .start(
                authorized_request(SubagentContinuation::Continuable, authority.clone()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = registry
            .start(
                authorized_request(SubagentContinuation::Continuable, authority),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), SubagentErrorCode::Refused);
        assert_eq!(registry.live_child_count(), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn declarative_preset_registration_is_exactly_disposable() {
        let registry = SubagentRegistry::new();
        let preset = SubagentPreset::new(
            "reviewer",
            "Careful reviewer",
            "Review correctness and safety.",
            Some(SubagentProviderId::new("native").unwrap()),
            SubagentSeed::Fresh,
            SubagentContinuation::OneShot,
        )
        .unwrap();
        let registration = registry.register_preset_owned(preset.clone()).unwrap();
        assert_eq!(registry.presets(), [preset]);
        assert!(registry.preset("reviewer").is_some());
        assert!(
            registry
                .register_preset_owned(registry.preset("reviewer").unwrap())
                .is_err()
        );

        drop(registration);
        assert!(registry.presets().is_empty());
        assert!(registry.preset("reviewer").is_none());
    }
    #[tokio::test]
    async fn named_message_routes_preserve_control_scope_and_pin_reused_names() {
        let registry = SubagentRegistry::new();
        registry
            .register(Arc::new(OwnedProvider {
                descriptor: descriptor(SubagentCapabilities {
                    fork: CapabilitySupport::Supported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                }),
                starts: AtomicUsize::new(0),
            }))
            .unwrap();
        let root = registry.root_authority(SubagentId::new("main-session").unwrap());
        let foreign = registry.root_authority(SubagentId::new("foreign-session").unwrap());
        let mut ids = Vec::new();
        for (authority, label) in [(&root, "Atlas"), (&root, "Boreal"), (&foreign, "Foreign")] {
            ids.push(
                registry
                    .start(
                        SubagentRequest::with_authority(
                            label,
                            "research",
                            SubagentSeed::Fresh,
                            SubagentContinuation::Continuable,
                            authority.clone(),
                        )
                        .unwrap(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap()
                    .id,
            );
        }
        let atlas = registry.authority_for_child(&root, &ids[0]).unwrap();
        assert_eq!(
            registry.resolve_message_target(&atlas, "parent").unwrap(),
            *root.owner()
        );
        assert_eq!(
            registry.resolve_message_target(&atlas, "main").unwrap(),
            *root.owner()
        );
        assert_eq!(
            registry.resolve_message_target(&atlas, "Boreal").unwrap(),
            ids[1]
        );
        assert!(
            registry.child_for(&atlas, &ids[1]).is_none(),
            "messaging does not confer sibling control"
        );
        assert!(
            registry
                .resolve_message_target(&atlas, ids[2].as_str())
                .is_err()
        );
        assert!(
            registry
                .resolve_message_target(&foreign, ids[0].as_str())
                .is_err()
        );
        let nested = registry
            .start(
                SubagentRequest::with_authority(
                    "Nested",
                    "research",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    atlas.clone(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .id;
        let nested_authority = registry.authority_for_child(&atlas, &nested).unwrap();
        assert_eq!(
            registry
                .resolve_message_target(&nested_authority, "parent")
                .unwrap(),
            ids[0]
        );
        assert_eq!(
            registry
                .resolve_message_target(&nested_authority, "main")
                .unwrap(),
            *root.owner()
        );
        assert!(
            registry
                .resolve_message_target(&nested_authority, "Boreal")
                .is_err()
        );
        assert_eq!(
            registry.resolve_message_target(&root, "Atlas").unwrap(),
            ids[0]
        );
        registry
            .task_record_for(&root, &ids[0])
            .unwrap()
            .update(|row| row.label = "Renamed Atlas".into())
            .unwrap();
        let replacement = registry
            .start(
                SubagentRequest::with_authority(
                    "Atlas",
                    "replacement",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    root.clone(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .id;
        assert!(
            registry.resolve_message_target(&root, "Atlas").is_err(),
            "an old name must not silently retarget"
        );
        assert_eq!(
            registry
                .resolve_message_target(&root, replacement.as_str())
                .unwrap(),
            replacement
        );
    }
    struct InspectRunHandle {
        id: SubagentId,
        record: Arc<crate::task_inventory::TaskRecord>,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl SubagentHandle for InspectRunHandle {
        fn id(&self) -> &SubagentId {
            &self.id
        }
        fn label(&self) -> &str {
            "inspect run"
        }
        async fn send(
            &self,
            _text: &str,
            _token: CancellationToken,
        ) -> Result<String, SubagentError> {
            let before = self.record.read().job_id;
            self.release.notified().await;
            assert_eq!(
                self.record.read().job_id,
                before,
                "queued work must not overwrite the active invocation identity"
            );
            Ok("done".into())
        }
        fn interrupt(&self) -> bool {
            false
        }
        async fn close(&self, _: CancellationToken) -> Result<(), SubagentError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn queued_invocation_identity_changes_only_under_the_retained_turn_lease() {
        let registry = SubagentRegistry::new();
        registry
            .register(Arc::new(OwnedProvider {
                descriptor: descriptor(SubagentCapabilities {
                    fork: CapabilitySupport::Supported,
                    continuation: CapabilitySupport::Supported,
                    interrupt: CapabilitySupport::Supported,
                }),
                starts: AtomicUsize::new(0),
            }))
            .unwrap();
        let authority = registry.root_authority(SubagentId::new("owner").unwrap());
        let started = registry
            .start(
                SubagentRequest::with_authority(
                    "Worker",
                    "start",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    authority.clone(),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let record = registry.task_record_for(&authority, &started.id).unwrap();
        let inner = Arc::new(InspectRunHandle {
            id: started.id.clone(),
            record: record.clone(),
            release: tokio::sync::Notify::new(),
        });
        let alias = AliasedHandle {
            id: started.id,
            inner: inner.clone(),
            record: record.clone(),
        };
        let first_id = JobId::parse("job-11").unwrap();
        let second_id = JobId::parse("job-12").unwrap();
        let first = alias.send_in_job("first", &first_id, CancellationToken::new());
        let second = alias.send_in_job("second", &second_id, CancellationToken::new());
        tokio::pin!(first, second);
        assert!(matches!(
            futures::poll!(&mut first),
            std::task::Poll::Pending
        ));
        assert_eq!(record.read().job_id.as_deref(), Some("job-11"));
        assert!(matches!(
            futures::poll!(&mut second),
            std::task::Poll::Pending
        ));
        assert_eq!(record.read().job_id.as_deref(), Some("job-11"));
        inner.release.notify_one();
        first.await.unwrap();
        assert!(matches!(
            futures::poll!(&mut second),
            std::task::Poll::Pending
        ));
        assert_eq!(record.read().job_id.as_deref(), Some("job-12"));
        inner.release.notify_one();
        second.await.unwrap();
    }

    struct LateInboxProvider {
        descriptor: SubagentProviderDescriptor,
        agent: Arc<Agent>,
        release: tokio::sync::Notify,
    }
    struct ExactInboxHandle {
        id: SubagentId,
        agent: Arc<Agent>,
    }
    #[async_trait]
    impl SubagentHandle for ExactInboxHandle {
        fn id(&self) -> &SubagentId {
            &self.id
        }
        fn label(&self) -> &str {
            "Late worker"
        }
        async fn send(&self, _: &str, _: CancellationToken) -> Result<String, SubagentError> {
            panic!("native message provenance must not use fresh text fallback")
        }
        async fn run_pending(
            &self,
            id: &heycode_session::InboxMessageId,
            token: CancellationToken,
        ) -> Result<String, SubagentError> {
            self.agent
                .send_inbox_id_cancellable(id, token)
                .await
                .map(|report| report.text)
                .map_err(|error| SubagentError::new(SubagentErrorCode::Failed, error.to_string()))
        }
        fn interrupt(&self) -> bool {
            false
        }
        async fn close(&self, _: CancellationToken) -> Result<(), SubagentError> {
            Ok(())
        }
    }
    #[async_trait]
    impl SubagentProvider for LateInboxProvider {
        fn descriptor(&self) -> &SubagentProviderDescriptor {
            &self.descriptor
        }
        fn inherits_parent_tool_guards(&self) -> bool {
            true
        }
        async fn start(
            &self,
            request: SubagentRequest,
            _: CancellationToken,
        ) -> Result<SubagentStarted, SubagentError> {
            self.release.notified().await;
            let record = request.task.unwrap();
            record
                .publish_native(
                    self.agent.session().lock().unwrap().id().to_string(),
                    &self.agent,
                )
                .unwrap();
            let id = SubagentId::new(record.read().id).unwrap();
            Ok(SubagentStarted {
                id: id.clone(),
                text: "started".into(),
                handle: Some(Arc::new(ExactInboxHandle {
                    id,
                    agent: self.agent.clone(),
                })),
            })
        }
    }

    #[tokio::test]
    async fn messages_queued_before_native_startup_keep_typed_sender_provenance() {
        let (mut parent_context, _parent_dir, parent) =
            crate::jobs::agent_completion_tests::fixture();
        let (mut child_context, _child_dir, child) = crate::jobs::agent_completion_tests::fixture();
        let registry = Arc::new(SubagentRegistry::new());
        let jobs = Arc::new(JobRegistry::new(0));
        registry
            .attach_job_host(&parent_context, &parent, jobs.clone())
            .unwrap();
        let provider = Arc::new(LateInboxProvider {
            descriptor: descriptor(SubagentCapabilities {
                fork: CapabilitySupport::Supported,
                continuation: CapabilitySupport::Supported,
                interrupt: CapabilitySupport::Supported,
            }),
            agent: child.clone(),
            release: tokio::sync::Notify::new(),
        });
        registry.register(provider.clone()).unwrap();
        let authority = registry.root_authority(
            SubagentId::new(parent.session().lock().unwrap().id().to_string()).unwrap(),
        );
        let (id, _) = registry
            .start_background_task(
                SubagentRequest::with_authority(
                    "Late worker",
                    "start",
                    SubagentSeed::Fresh,
                    SubagentContinuation::Continuable,
                    authority.clone(),
                )
                .unwrap(),
                InboxDelivery::Inject,
            )
            .unwrap();
        let source = heycode_session::InboxSource::Agent {
            agent_id: authority.owner().to_string(),
            agent_name: "main".into(),
            recipient_id: id.to_string(),
            run_id: "run-0".into(),
            completion_id: None,
            outcome: None,
        };
        let receipt = registry
            .queue_message_from(
                &authority,
                &id,
                "preserve this finding".into(),
                false,
                InboxDelivery::Inject,
                Some((authority.clone(), source.clone())),
            )
            .unwrap();
        assert!(
            receipt.message_id.is_none(),
            "runtime has not published its inbox yet"
        );
        provider.release.notify_one();
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                jobs.wait_for_task_exit(&receipt.job_id)
            )
            .await
            .unwrap()
            .unwrap(),
            JobOutcome::Completed
        );
        assert!(child.session().lock().unwrap().events().iter().any(|event| matches!(&event.kind, heycode_session::SessionEventKind::AgentInboxSplice { inserted, .. } if inserted.iter().any(|message| message.source() == &source))));
        assert_eq!(
            registry
                .task_record_for(&authority, &id)
                .unwrap()
                .read()
                .job_id,
            Some(receipt.job_id.to_string())
        );
        jobs.dispose();
        registry.dispose();
        parent_context.shutdown();
        child_context.shutdown();
    }
}
