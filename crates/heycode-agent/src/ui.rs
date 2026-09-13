//! Live UI event catalog.
//!
//! [`UiEvent`]s are transient: they carry in-flight progress that is NOT
//! durable. Replay-safe surfaces rebuild the transcript from `SessionEvent`s;
//! these events only animate the present moment (deltas, spinner verbs,
//! command output, quit requests).

/// Attachment-composer mutation requested by a human-only command.
#[derive(Debug, Clone)]
pub enum AttachmentComposerAction {
    /// Add one already-admitted image or document.
    Add(heycode_core::AttachmentMetadata),
    /// Remove every pending attachment.
    Clear,
}

/// Validated id of a human-only UI panel requested by a capability command.
///
/// The agent event bus carries the id without depending on a concrete front
/// end. A TUI may open a modal, while another client may project the same
/// request into its own navigation model. The id is deliberately opaque so a
/// plugin cannot smuggle terminal control text through a command request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UiPanelId(String);

/// Validation failure for [`UiPanelId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid UI panel id")]
pub struct UiPanelIdError;

impl UiPanelId {
    /// Validate a lowercase kebab-case panel id.
    ///
    /// # Errors
    /// Empty, oversized or malformed ids are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, UiPanelIdError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=128).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.contains("--");
        if valid {
            Ok(Self(value))
        } else {
            Err(UiPanelIdError)
        }
    }

    /// Stable lookup value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UiPanelId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Source-ordered child of the shared settings/status shell.
///
/// `Config` is backed by the front end's live Settings service. The other
/// children are immutable snapshots captured by the command authority, so the
/// shell never reconstructs runtime truth or performs a credentialed fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingsShellTab {
    /// Effective runtime, route, sandbox and health diagnostics.
    Status,
    /// Live schema-derived settings browser.
    Config,
    /// Durable current-session token, route and derived-cost facts.
    Usage,
    /// Cross-session activity statistics, when an aggregate owner exists.
    Stats,
}

impl SettingsShellTab {
    /// Selectable tabs in source order.
    pub const ALL: [Self; 4] = [Self::Status, Self::Config, Self::Usage, Self::Stats];

    /// Human label used by native and flat clients.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Config => "Config",
            Self::Usage => "Usage",
            Self::Stats => "Stats",
        }
    }

    /// Select the next tab and wrap after Stats.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Status => Self::Config,
            Self::Config => Self::Usage,
            Self::Usage => Self::Stats,
            Self::Stats => Self::Status,
        }
    }

    /// Select the previous tab and wrap before Status.
    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Status => Self::Stats,
            Self::Config => Self::Status,
            Self::Usage => Self::Config,
            Self::Stats => Self::Usage,
        }
    }
}

/// One non-Config settings-shell section with an explicit evidence state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsShellSection {
    /// Authoritative snapshot text is available.
    Ready {
        /// Plain, terminal-control-free section text.
        text: String,
    },
    /// The owner exists and authoritatively observed no data yet.
    Empty {
        /// Human explanation that distinguishes empty from unavailable.
        message: String,
    },
    /// This build has no authority capable of producing the section.
    Unavailable {
        /// Stable limitation explanation; absence is not rendered as zero.
        reason: String,
    },
    /// The authority exists but its current read failed.
    Failed {
        /// Safe failure summary without credentials.
        message: String,
    },
}

impl SettingsShellSection {
    /// Text projection shared by native, flat and runtime-bridge fallbacks.
    #[must_use]
    pub fn plain_text(&self) -> &str {
        match self {
            Self::Ready { text } => text,
            Self::Empty { message } | Self::Failed { message } => message,
            Self::Unavailable { reason } => reason,
        }
    }
}

/// Immutable non-Config data captured for one settings-shell request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsShellSnapshot {
    /// Effective status and diagnostics.
    pub status: SettingsShellSection,
    /// Durable current-session usage.
    pub usage: SettingsShellSection,
    /// Cross-session activity statistics or an explicit missing-owner state.
    pub stats: SettingsShellSection,
    /// Structured provider-neutral facts for an interactive Stats child.
    ///
    /// Text-only clients keep using [`SettingsShellSection::plain_text`]. This
    /// optional typed projection lets richer clients build overview and model
    /// views without parsing presentation text or inventing account data.
    pub stats_snapshot: Option<Box<heycode_session::SessionStatsSnapshot>>,
}

impl SettingsShellSnapshot {
    /// Section for one non-Config tab.
    #[must_use]
    pub const fn section(&self, tab: SettingsShellTab) -> Option<&SettingsShellSection> {
        match tab {
            SettingsShellTab::Status => Some(&self.status),
            SettingsShellTab::Config => None,
            SettingsShellTab::Usage => Some(&self.usage),
            SettingsShellTab::Stats => Some(&self.stats),
        }
    }

    /// Honest text fallback for clients without an interactive panel.
    #[must_use]
    pub fn plain_text_for(&self, tab: SettingsShellTab) -> String {
        let body = self.section(tab).map_or(
            "Interactive settings are unavailable on this surface. Use /config show for effective configuration provenance.",
            SettingsShellSection::plain_text,
        );
        format!("settings\nactive: {}\n\n{body}", tab.label())
    }

    /// Structured local Stats facts when the aggregate owner returned data.
    #[must_use]
    pub fn stats_snapshot(&self) -> Option<&heycode_session::SessionStatsSnapshot> {
        self.stats_snapshot.as_deref()
    }
}

/// Backend that owns model and effort discovery/application for the active
/// loop. Keeping the two planes distinct prevents a delegated runtime picker
/// from querying or mutating the dormant native inference route.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackendControlOwner {
    /// Native inference provider and its adapter/catalog.
    NativeInference {
        /// Provider registry id.
        provider: String,
    },
    /// Delegated coding-agent runtime and its live backend control plane.
    DelegatedRuntime {
        /// Runtime registry id.
        runtime: String,
    },
}

impl BackendControlOwner {
    /// Stable provider/runtime registry id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::NativeInference { provider } => provider,
            Self::DelegatedRuntime { runtime } => runtime,
        }
    }

    /// Whether native inference owns the active loop.
    #[must_use]
    pub const fn is_native(&self) -> bool {
        matches!(self, Self::NativeInference { .. })
    }
}

/// One live progress update emitted on the shared event bus.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A turn began.
    TurnStarted {
        /// 1-based turn number within the session.
        turn: u64,
    },
    /// A delegated runtime turn with an opaque provider-native id began.
    RuntimeTurnStarted {
        /// Provider-native turn identity retained for diagnostics/correlation.
        turn_id: String,
    },
    /// The user's text was accepted for this turn.
    UserEcho {
        /// Verbatim user input.
        text: String,
    },
    /// Attachment selection was durably paired with the accepted user text.
    UserAttachmentsEcho {
        /// Selected durable image or resolved document metadata.
        attachments: Vec<heycode_core::AttachmentMetadata>,
        /// Explicit native/extracted route for every document.
        document_routes: Vec<heycode_core::DocumentInputRoute>,
    },
    /// Human command requests a pending-composer attachment mutation.
    AttachmentComposerRequested {
        /// Add or clear action.
        action: AttachmentComposerAction,
    },
    /// Assistant text streamed one fragment.
    AssistantDelta {
        /// Fragment to append to the streaming buffer.
        text: String,
    },
    /// Assistant audio output committed as immutable attachment metadata.
    AssistantAudio {
        /// One to four exact audio records; encoded bytes stay in ATT01.
        attachments: Vec<heycode_core::AttachmentMetadata>,
    },
    /// Reasoning content streamed one fragment (reasoning models).
    ReasoningDelta {
        /// Fragment to append to the reasoning buffer.
        text: String,
    },
    /// A tool call was dispatched.
    ToolStarted {
        /// Tool name.
        name: String,
        /// FULL argument object — front ends build their own views.
        args: serde_json::Value,
    },
    /// A tool call settled.
    ToolFinished {
        /// Tool name.
        name: String,
        /// False when denied or failed.
        ok: bool,
        /// FULL result value (string or JSON object incl. diffs) — front ends
        /// derive their own tails/views.
        value: serde_json::Value,
        /// Successful external content that remains data-only.
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    },
    /// Spinner verb update ("Reading…", "Forging…").
    Status {
        /// Verb shown next to the spinner glyph.
        verb: String,
    },
    /// The turn settled.
    TurnFinished {
        /// `stop | max_tokens | error | aborted`.
        reason: String,
        /// Token usage reported by the provider, when known.
        usage: Option<heycode_core::TokenUsage>,
        /// Rough estimate of the FULL context (prompt+history) in tokens,
        /// ~4 chars/token — drives the status-bar context meter.
        context_tokens: Option<u64>,
    },
    /// Current request budget, independent of billing usage.
    ContextBudgetChanged {
        /// Model capacity, measurement confidence and compaction state.
        budget: heycode_llm::ContextBudget,
    },
    /// A delegated runtime reported its exact current context occupancy.
    RuntimeContextMeasured {
        /// Concrete model identity reported by the runtime.
        resolved_model: Option<String>,
        /// Tokens currently occupying the model-visible context.
        tokens: u64,
        /// Effective context-window capacity for this measurement.
        context_window: u64,
    },
    /// An error the user should see; the turn may continue around it.
    Error {
        /// Human-readable message.
        message: String,
    },
    /// Command/side-channel output rendered as an info line.
    Info {
        /// Text to render.
        text: String,
    },
    /// A revision-verified finding report committed to durable session truth.
    FindingsReported {
        /// Complete bounded structured report; no external publication occurred.
        report: heycode_session::FindingReport,
    },
    /// A capability-owned command asks the active front end to open a panel.
    CapabilityPanelRequested {
        /// Stable capability panel id.
        panel: UiPanelId,
    },
    /// Open the shared settings/status shell on one source-mapped child.
    SettingsShellRequested {
        /// Child selected by `/status`, `/config`, `/usage`, or `/stats`.
        tab: SettingsShellTab,
        /// Immutable authoritative data for every non-Config child.
        snapshot: SettingsShellSnapshot,
    },
    /// Browse the live command catalog without adding model-visible history.
    HelpRequested {
        /// Current route summary for text-only clients.
        header: String,
        /// One availability snapshot, including aliases and plugin commands.
        commands: Vec<crate::CommandCatalogEntry>,
    },
    /// Open the strict named-profile picker.
    ProfilePickerRequested,
    /// Apply one validated named-profile selection through recomposition.
    ProfileSelected {
        /// Stable profile file stem.
        name: String,
    },
    /// Open the live model picker for the effective route.
    ModelPickerRequested {
        /// Active backend that must serve discovery and application.
        owner: BackendControlOwner,
        /// Routing revision captured with this picker request.
        routing_revision: u64,
        /// Current model highlighted when present.
        current_model: String,
    },
    /// Open the exact effort picker for the active backend.
    EffortPickerRequested {
        /// Active backend that owns these choices and their application.
        owner: BackendControlOwner,
        /// Routing revision captured with this picker request.
        routing_revision: u64,
        /// Explicit current value; absence means backend/provider default.
        current_effort: Option<String>,
        /// Exact accepted ids in backend display order.
        choices: Vec<String>,
        /// Explicit backend default when known.
        default_effort: Option<String>,
    },
    /// Open the combined inference-provider/agent-runtime picker.
    RoutePickerRequested {
        /// Effective inference provider inside the native loop.
        current_provider: String,
        /// Effective top-level AgentRuntime id.
        current_runtime: String,
    },
    /// Open the permission/sandbox picker from the effective execution-policy report.
    PermissionPickerRequested {
        /// Complete live capability report; never reconstructed from requested config.
        report: heycode_exec::SandboxCapabilityReport,
    },
    /// Inspect the effective sandbox policy in a tabbed terminal panel.
    SandboxPanelRequested {
        /// Authoritative execution-service capabilities.
        report: heycode_exec::SandboxCapabilityReport,
    },
    /// Open the settings-backed automatic compaction window picker.
    AutoCompactPickerRequested {
        /// Whether the active profile enables automatic compaction.
        enabled: bool,
        /// Effective fixed window, or model-aware automatic policy.
        current_tokens: Option<u64>,
    },
    /// Reopen the plugin-owned connection wizard after `/connect`.
    ConnectRequested,
    /// Logout retired the active route and requires a fresh composition/setup.
    LoggedOut {
        /// Active provider or delegated runtime that was disconnected.
        target: String,
        /// Safe credential-cleanup failure to surface after recomposition.
        cleanup_warning: Option<String>,
    },
    /// The user asked to exit; the shell owns quitting.
    QuitRequested,
    /// An effective session permission transition has committed.
    PermissionModeChanged {
        /// Actual permission authority, not an optimistic picker label.
        mode: crate::ApprovalPolicyKind,
    },
    /// Dedicated full-document Plan review, never an ordinary tool grant.
    PlanReviewRequested {
        /// Review correlation id.
        id: u64,
        /// Complete Markdown document.
        plan: String,
    },
    /// Review waiter settled or was cancelled.
    PlanReviewResolved {
        /// Review correlation id.
        id: u64,
    },
    /// A tool call awaits an interactive approval decision.
    ApprovalRequested {
        /// Child session that owns the request; None denotes the root.
        owner_session: Option<String>,
        /// Id to pass back through the approval service.
        id: u64,
        /// Tool name.
        name: String,
        /// Argument preview for the card.
        args_preview: String,
    },
    /// A previously requested approval settled (transcript echo).
    ApprovalResolved {
        /// Matching request id.
        id: u64,
        /// Whether the human allowed the call.
        allowed: bool,
    },
    /// A delegated runtime operation awaits a human permission decision.
    RuntimePermissionRequested {
        /// Provider-native request correlation.
        request_id: String,
        /// Safe action title.
        action: String,
        /// Safe bounded detail.
        detail: String,
    },
    /// Pending operational inbox state changed, or a turn settled with work
    /// still queued. `wake` states whether an owner must start a turn.
    InboxUpdated {
        /// Inputs waiting for a new turn.
        next_turn: usize,
        /// Inputs waiting for the next step boundary.
        next_step: usize,
        /// Whether the receiver owes exactly one follow-up turn.
        wake: crate::InboxWake,
    },
    /// A durable optional question was admitted; work continues without stealing focus.
    OptionalQuestionRequested {
        /// Explicit answer interaction mode.
        mode: heycode_core::QuestionMode,
        /// Short category label.
        header: Option<String>,
        /// Explanations aligned with suggested labels.
        choice_descriptions: Vec<Option<String>>,
        /// Exact originating session, distinct from a child task handle.
        session_id: String,
        /// Durable question and answer correlation.
        question_id: String,
        /// Human-visible prompt.
        prompt: String,
        /// Suggested choices; custom text is always allowed.
        choices: Vec<String>,
    },
    /// A durable optional question was answered or explicitly dismissed.
    OptionalQuestionSettled {
        /// Originating session.
        session_id: String,
        /// Settled durable question.
        question_id: String,
    },
    /// A delegated runtime asks one bounded non-secret human question.
    RuntimeQuestionRequested {
        /// Exact requesting session, absent when an older transport cannot provide it.
        owner_session_id: Option<String>,
        /// Explicit answer interaction mode.
        mode: heycode_core::QuestionMode,
        /// Position and total in a question batch.
        progress: (usize, usize),
        /// Provider-native request correlation.
        request_id: String,
        /// Optional short category label.
        header: Option<String>,
        /// Safe prompt text.
        prompt: String,
        /// Ordered choices; empty permits free text.
        choices: Vec<String>,
        /// Explanations aligned one-for-one with `choices`.
        choice_descriptions: Vec<Option<String>>,
    },
}

impl UiEvent {
    /// Spinner verb implied by an event, if any.
    #[must_use]
    pub fn verb(&self) -> Option<&str> {
        match self {
            Self::Status { verb } => Some(verb),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::UiPanelId;

    #[test]
    fn panel_ids_are_opaque_and_control_free() {
        assert_eq!(
            UiPanelId::new("plugin-health").unwrap().as_str(),
            "plugin-health"
        );
        for malformed in ["", "Plugins", "two words", "two--dashes", "escape\u{1b}"] {
            assert!(UiPanelId::new(malformed).is_err(), "{malformed:?}");
        }
    }
}
