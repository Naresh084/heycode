//! Session log vocabulary: the JSONL envelope and its closed kind set.

use serde::{Deserialize, Serialize};

pub use heycode_core::TokenUsage;

/// Oldest envelope version this build can migrate on read.
pub const MIN_SESSION_LOG_VERSION: u8 = 1;
/// Envelope version used for every new append.
pub const CURRENT_SESSION_LOG_VERSION: u8 = 2;

/// Every `kind` tag the reader accepts; anything else fails resume loudly
/// instead of skipping (AGENTS.md §4: closed set, unknown ⇒ hard error).
pub(crate) const KNOWN_KINDS_V1: &[&str] = &[
    "turn/start",
    "turn/end",
    "step/start",
    "step/end",
    "user/message",
    "assistant/chunk",
    "assistant/message",
    "tool/call",
    "tool/result",
    "compaction/applied",
    "plan/mode",
    "session/title",
];

/// V2 starts with the v1 kind set; C02/C03 add v2-only kinds deliberately.
pub(crate) const KNOWN_KINDS_V2: &[&str] = &[
    "session/created",
    "session/activated",
    "runtime/linked",
    "runtime/configured",
    "turn/start",
    "turn/end",
    "step/start",
    "step/end",
    "agent/inbox/splice",
    "goal/change",
    "workflow/change",
    "schedule/change",
    "team/change",
    "work/change",
    "review/change",
    "code-mode/change",
    "hook/contribution",
    "request/header",
    "request/context",
    "user/message",
    "user/attachments",
    "attachment/added",
    "assistant/chunk",
    "assistant/message",
    "assistant/audio",
    "assistant/provider-item",
    "assistant/response-metadata",
    "server-tool/call",
    "server-tool/result",
    "server-tool/usage",
    "assistant/citation",
    "tool/call",
    "tool/result",
    "tool/rich-result",
    "compaction/applied",
    "compaction/native",
    "plan/mode",
    "plan/review",
    "session/title",
];

/// Why a turn stopped. Serialized snake_case (`max_tokens`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    /// The model finished on its own.
    Stop,
    /// A token limit cut the turn short.
    MaxTokens,
    /// Explicit per-turn provider-step budget was exhausted.
    MaxSteps,
    /// Explicit per-turn elapsed-time budget was exhausted.
    MaxElapsed,
    /// Explicit per-turn client-tool budget was exhausted.
    MaxToolCalls,
    /// Strict token accounting could not continue without reported usage.
    UnreportedTokenUsage,
    /// Elapsed policy could not obtain a safe clock reading.
    ClockUnavailable,
    /// The turn ended because of an error.
    Error,
    /// The user or system aborted the turn.
    Aborted,
}

/// Durable outcome of sending one complete configuration to a runtime.
///
/// `Committed` is the default so logs written before this field existed keep
/// their original meaning when replayed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigurationState {
    /// The exact configuration is durable, but the runtime has not acknowledged it.
    Attempted,
    /// The runtime acknowledged this exact effective configuration.
    #[default]
    Committed,
    /// The runtime rejected the attempt or returned an incoherent result.
    Failed,
}

impl RuntimeConfigurationState {
    /// Whether replay may treat this configuration as effective.
    #[must_use]
    pub const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed)
    }
}

/// One model tool call as recorded on the log / sent to providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallOut {
    /// Provider-side call id echoed back by the matching tool result.
    pub id: String,
    /// Tool name as declared in the registry.
    pub name: String,
    /// Raw JSON text of arguments, verbatim from the model.
    pub arguments: String,
}

/// Token accounting for one assistant completion step.
/// The closed set of session event kinds (AGENTS.md §4).
///
/// Serialized adjacently tagged: each envelope line carries
/// `"kind": "<tag>"` and `"data": { <variant fields> }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum SessionEventKind {
    /// Immutable creation metadata for one physical session stream.
    #[serde(rename = "session/created")]
    SessionCreated {
        /// Safe local metadata plus optional verified parent prefix.
        creation: Box<crate::SessionCreation>,
    },

    /// A human opened this conversation; never included in model requests.
    #[serde(rename = "session/activated")]
    SessionActivated {},

    /// Bind this durable heycode stream to one provider-native runtime session.
    #[serde(rename = "runtime/linked")]
    RuntimeLinked {
        /// AgentRuntimeRegistry id.
        runtime: String,
        /// Provider-native thread/session id used for resume.
        runtime_session_id: String,
    },

    /// Exact model-visible controls recorded before a child runtime receives them,
    /// followed by the runtime's durable acknowledgement outcome.
    #[serde(rename = "runtime/configured")]
    RuntimeConfigured {
        /// Whether the child runtime acknowledged this exact configuration.
        #[serde(
            default,
            skip_serializing_if = "RuntimeConfigurationState::is_committed"
        )]
        state: RuntimeConfigurationState,
        /// Complete system prompt supplied to the runtime.
        #[serde(skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        /// Ordered host tools and their exact input schemas.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<heycode_core::ToolSpec>>,
        /// Provider-native model id.
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Provider-native reasoning effort id.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
    },

    /// A turn began.
    #[serde(rename = "turn/start")]
    TurnStart {
        /// Zero-based turn index within the session.
        turn: u64,
    },

    /// A turn ended.
    #[serde(rename = "turn/end")]
    TurnEnd {
        /// Zero-based turn index within the session.
        turn: u64,
        /// Why the turn stopped.
        reason: TurnEndReason,
    },

    /// One loop iteration inside a turn began.
    #[serde(rename = "step/start")]
    StepStart {
        /// Zero-based turn index within the session.
        turn: u64,
        /// Zero-based step index within the turn.
        step: u32,
    },

    /// One loop iteration inside a turn ended.
    #[serde(rename = "step/end")]
    StepEnd {
        /// Zero-based turn index within the session.
        turn: u64,
        /// Zero-based step index within the turn.
        step: u32,
    },

    /// One normalized mutation of an agent's durable pending-input lists.
    #[serde(rename = "agent/inbox/splice")]
    AgentInboxSplice {
        /// Pending list mutated by this splice.
        target: crate::InboxTarget,
        /// Zero-based normalized insertion/removal position.
        start: u32,
        /// Number of existing messages removed; absent means zero.
        #[serde(skip_serializing_if = "Option::is_none")]
        removed_count: Option<u32>,
        /// Messages inserted at `start`, in order.
        inserted: Vec<crate::InboxMessage>,
        /// Present only when removed work was canceled instead of claimed.
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<crate::InboxSpliceOutcome>,
    },

    /// One revisioned full-snapshot goal mutation or clear tombstone.
    #[serde(rename = "goal/change")]
    GoalChange {
        /// Strict version-one goal-domain payload.
        change: Box<crate::GoalChange>,
    },

    /// One versioned workflow lifecycle/progress/checkpoint mutation.
    #[serde(rename = "workflow/change")]
    WorkflowChange {
        /// Strict version-one workflow payload.
        change: Box<crate::WorkflowChange>,
    },

    /// One revision-safe structured work mutation.
    #[serde(rename = "work/change")]
    WorkChange {
        /// Strict work-domain payload.
        change: Box<crate::WorkChange>,
    },

    /// One versioned session-local schedule mutation.
    #[serde(rename = "schedule/change")]
    ScheduleChange {
        /// Strict version-one schedule payload.
        change: Box<crate::ScheduleChange>,
    },

    /// One revisioned roster/task-DAG/mailbox mutation.
    #[serde(rename = "team/change")]
    TeamChange {
        /// Strict version-one team-domain payload.
        change: Box<crate::TeamChange>,
    },

    /// One exact review input or structured terminal settlement.
    #[serde(rename = "review/change")]
    ReviewChange {
        /// Strict version-one review-domain payload.
        change: Box<crate::ReviewChange>,
    },

    /// Durable JavaScript orchestration and nested tool evidence.
    #[serde(rename = "code-mode/change")]
    CodeModeChange {
        /// Versioned script lifecycle payload.
        change: Box<crate::CodeModeChange>,
    },

    /// Bounded hook-produced context after its durable bridge committed.
    #[serde(rename = "hook/contribution")]
    HookContribution {
        /// Exact value-free provenance and contribution text.
        contribution: Box<crate::HookContributionRecord>,
    },

    /// Complete resolved route/prompt/tool/options snapshot before dispatch.
    #[serde(rename = "request/header")]
    RequestHeader {
        /// Turn this request belongs to.
        turn: u64,
        /// Step this request belongs to.
        step: u32,
        /// Correlates header/context/provider items and terminal outcome.
        request_id: heycode_core::RequestId,
        /// Complete validated header.
        header: Box<crate::RequestHeaderSnapshot>,
    },

    /// Correctness-sensitive context/catalog evidence for a request.
    #[serde(rename = "request/context")]
    RequestContext {
        /// Request correlation id.
        request_id: heycode_core::RequestId,
        /// Validated capacity/catalog context.
        context: crate::RequestContextSnapshot,
    },

    /// The user submitted a message.
    #[serde(rename = "user/message")]
    UserMessage {
        /// Verbatim user input text.
        text: String,
    },

    /// Attachments selected for the immediately following user message.
    #[serde(rename = "user/attachments")]
    UserAttachments {
        /// One to sixteen validated durable attachment records.
        attachments: Vec<heycode_core::AttachmentMetadata>,
        /// Explicit native-versus-extracted routes for every document row.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        document_routes: Vec<heycode_core::DocumentInputRoute>,
    },

    /// One content-addressed attachment became durable for this session.
    #[serde(rename = "attachment/added")]
    AttachmentAdded {
        /// Validated safe metadata; bytes live in the attachment store.
        attachment: Box<heycode_core::AttachmentMetadata>,
    },

    /// Presentational streaming fragment; ignorable on replay.
    #[serde(rename = "assistant/chunk")]
    AssistantChunk {
        /// Turn this chunk belongs to.
        turn: u64,
        /// Step this chunk belongs to.
        step: u32,
        /// Incremental visible text, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Incremental reasoning text, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },

    /// One complete assistant message (possibly tool-call-only).
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        /// Turn this message belongs to.
        turn: u64,
        /// Step this message belongs to.
        step: u32,
        /// Visible text content; may be empty when only tool calls are present.
        content: String,
        /// Full reasoning text, when the provider surfaced it.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        /// Tool calls requested by the model, in provider order.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCallOut>>,
        /// Token usage reported for the producing request, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },

    /// Audio output committed through ATT01 after terminal provider success.
    #[serde(rename = "assistant/audio")]
    AssistantAudio {
        /// Turn this output belongs to.
        turn: u64,
        /// Step this output belongs to.
        step: u32,
        /// Producing durable request.
        request_id: heycode_core::RequestId,
        /// One to four exact prior audio admissions; never inline bytes.
        attachments: Vec<heycode_core::AttachmentMetadata>,
    },

    /// Lossless provider-owned continuation item emitted by one request.
    #[serde(rename = "assistant/provider-item")]
    AssistantProviderItem {
        /// Turn this item belongs to.
        turn: u64,
        /// Step this item belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Ordered provider output index.
        output_index: u32,
        /// Validated lossless provider item.
        item: Box<heycode_core::ProviderStateItem>,
    },

    /// Neutral detailed usage/context-edit facts from one successful request.
    #[serde(rename = "assistant/response-metadata")]
    AssistantResponseMetadata {
        /// Turn this response belongs to.
        turn: u64,
        /// Step this response belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Validated provider-neutral detailed facts.
        metadata: Box<heycode_core::ProviderResponseMetadata>,
    },

    /// One provider-executed tool call normalized for replay/UI inspection.
    #[serde(rename = "server-tool/call")]
    ServerToolCall {
        /// Turn this provider call belongs to.
        turn: u64,
        /// Step this provider call belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Provider output/block index.
        output_index: u32,
        /// Validated provider-neutral call metadata.
        call: Box<heycode_core::ServerToolCall>,
    },

    /// One provider-executed tool result normalized without raw response data.
    #[serde(rename = "server-tool/result")]
    ServerToolResult {
        /// Turn this provider result belongs to.
        turn: u64,
        /// Step this provider result belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Provider output/block index.
        output_index: u32,
        /// Validated provider-neutral result metadata.
        result: Box<heycode_core::ServerToolResult>,
    },

    /// Provider-reported aggregate server-tool usage without synthetic calls.
    #[serde(rename = "server-tool/usage")]
    ServerToolUsage {
        /// Turn this aggregate belongs to.
        turn: u64,
        /// Step this aggregate belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Validated aggregate usage and cost evidence.
        usage: Box<heycode_core::ServerToolUsage>,
    },

    /// One public URL citation attached to assistant output.
    #[serde(rename = "assistant/citation")]
    AssistantCitation {
        /// Turn this citation belongs to.
        turn: u64,
        /// Step this citation belongs to.
        step: u32,
        /// Producing request correlation id.
        request_id: heycode_core::RequestId,
        /// Provider output/block index containing the citation.
        output_index: u32,
        /// Validated UI-safe citation metadata.
        citation: Box<heycode_core::UrlCitation>,
    },

    /// The agent dispatched a tool call for execution.
    #[serde(rename = "tool/call")]
    ToolCall {
        /// Turn this dispatch belongs to.
        turn: u64,
        /// Provider call id this execution answers.
        call_id: heycode_core::CallId,
        /// Executed tool name.
        name: String,
        /// Parsed arguments as submitted to the tool.
        args: serde_json::Value,
    },

    /// A tool execution outcome.
    #[serde(rename = "tool/result")]
    ToolResult {
        /// Provider call id this result answers.
        call_id: heycode_core::CallId,
        /// Verbatim tool output text.
        content: String,
        /// True when the tool reported failure.
        is_error: bool,
        /// External result content that must remain data-only in model/UI.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    },

    /// A typed rich tool result whose media committed through ATT01 first.
    #[serde(rename = "tool/rich-result")]
    RichToolResult {
        /// Provider call id this result answers.
        call_id: heycode_core::CallId,
        /// Complete ordered durable result.
        result: Box<heycode_core::DurableToolResult>,
        /// True when the remote tool reported an execution failure.
        is_error: bool,
        /// External result content that remains data-only in model/UI.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    },

    /// Context compaction replaced earlier history with a summary.
    #[serde(rename = "compaction/applied")]
    CompactionApplied {
        /// Summary text replacing the compacted span.
        summary: String,
        /// Highest seq covered by the compaction (inclusive).
        replaced_upto_seq: u64,
    },

    /// Provider-native compaction replaced a prefix with exact opaque state.
    #[serde(rename = "compaction/native")]
    NativeCompactionApplied {
        /// Agent-owned strategy id that produced the checkpoint.
        strategy: String,
        /// Highest seq covered by the checkpoint (inclusive).
        replaced_upto_seq: u64,
        /// Ordered same-route provider state used for continuation.
        items: Vec<heycode_core::ProviderStateItem>,
        /// Exact normalized provider usage, when available.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },

    /// Plan-mode switch; last-wins fold, enforced by guard layers.
    #[serde(rename = "plan/mode")]
    PlanMode {
        /// Whether plan mode is active after this event.
        active: bool,
    },

    /// Full proposal and explicit review decision; accepted decisions also commit plan exit and target policy.
    #[serde(rename = "plan/review")]
    PlanReview {
        /// Complete Markdown document; never a truncated preview.
        plan: String,
        /// Pending, accepted_edits, default, or stay_in_plan.
        decision: String,
        /// Human feedback retained across revisions and resume.
        feedback: String,
    },

    /// Log-only session title (never provider-visible).
    #[serde(rename = "session/title")]
    SessionTitle {
        /// The generated or user-set title.
        title: String,
    },
}

impl SessionEventKind {
    /// Stable wire tag of this kind (`"turn/start"`, ...), used in errors/logs.
    ///
    /// Duplicates the serde renames by necessity; the drift test in this
    /// module pins both sides to identical strings for every variant.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session/created",
            Self::SessionActivated {} => "session/activated",
            Self::RuntimeLinked { .. } => "runtime/linked",
            Self::RuntimeConfigured { .. } => "runtime/configured",
            Self::TurnStart { .. } => "turn/start",
            Self::TurnEnd { .. } => "turn/end",
            Self::StepStart { .. } => "step/start",
            Self::StepEnd { .. } => "step/end",
            Self::AgentInboxSplice { .. } => "agent/inbox/splice",
            Self::GoalChange { .. } => "goal/change",
            Self::WorkflowChange { .. } => "workflow/change",
            Self::ScheduleChange { .. } => "schedule/change",
            Self::TeamChange { .. } => "team/change",
            Self::WorkChange { .. } => "work/change",
            Self::ReviewChange { .. } => "review/change",
            Self::CodeModeChange { .. } => "code-mode/change",
            Self::HookContribution { .. } => "hook/contribution",
            Self::RequestHeader { .. } => "request/header",
            Self::RequestContext { .. } => "request/context",
            Self::UserMessage { .. } => "user/message",
            Self::UserAttachments { .. } => "user/attachments",
            Self::AttachmentAdded { .. } => "attachment/added",
            Self::AssistantChunk { .. } => "assistant/chunk",
            Self::AssistantMessage { .. } => "assistant/message",
            Self::AssistantAudio { .. } => "assistant/audio",
            Self::AssistantProviderItem { .. } => "assistant/provider-item",
            Self::AssistantResponseMetadata { .. } => "assistant/response-metadata",
            Self::ServerToolCall { .. } => "server-tool/call",
            Self::ServerToolResult { .. } => "server-tool/result",
            Self::ServerToolUsage { .. } => "server-tool/usage",
            Self::AssistantCitation { .. } => "assistant/citation",
            Self::ToolCall { .. } => "tool/call",
            Self::ToolResult { .. } => "tool/result",
            Self::RichToolResult { .. } => "tool/rich-result",
            Self::CompactionApplied { .. } => "compaction/applied",
            Self::NativeCompactionApplied { .. } => "compaction/native",
            Self::PlanMode { .. } => "plan/mode",
            Self::PlanReview { .. } => "plan/review",
            Self::SessionTitle { .. } => "session/title",
        }
    }

    pub(crate) fn validate_for_version(&self, version: u8) -> Result<(), String> {
        match self {
            Self::PlanReview { plan, decision, .. } => {
                if version < 2
                    || !plan.trim_start().starts_with("# ")
                    || !matches!(
                        decision.as_str(),
                        "pending" | "accepted_edits" | "default" | "stay_in_plan"
                    )
                {
                    return Err("invalid plan/review record or envelope version".into());
                }
                Ok(())
            }
            Self::SessionCreated { creation } => {
                if version < 2 {
                    return Err("session/created requires session envelope v2".to_owned());
                }
                creation.validate().map_err(|error| error.to_string())
            }
            Self::RuntimeLinked {
                runtime,
                runtime_session_id,
            } => {
                if version < 2 {
                    return Err("runtime/linked requires session envelope v2".to_owned());
                }
                if !crate::creation::valid_runtime_id(runtime)
                    || runtime_session_id.is_empty()
                    || runtime_session_id.len() > 256
                    || runtime_session_id.trim() != runtime_session_id
                    || runtime_session_id.chars().any(char::is_control)
                {
                    return Err("runtime/linked identity is invalid".to_owned());
                }
                Ok(())
            }
            Self::RuntimeConfigured {
                state: _,
                system_prompt,
                tools,
                model,
                reasoning_effort,
            } => {
                if version < 2 {
                    return Err("runtime/configured requires session envelope v2".to_owned());
                }
                if system_prompt.as_ref().is_some_and(|prompt| {
                    prompt.trim().is_empty() || prompt.len() > 1024 * 1024 || prompt.contains('\0')
                }) || model
                    .as_ref()
                    .is_some_and(|value| !valid_runtime_control(value, 256))
                    || reasoning_effort
                        .as_ref()
                        .is_some_and(|value| !valid_runtime_control(value, 128))
                    || tools
                        .as_ref()
                        .is_some_and(|tools| !valid_runtime_tools(tools))
                {
                    return Err("runtime/configured value is invalid".to_owned());
                }
                Ok(())
            }
            Self::RequestHeader { header, .. } => {
                if version < 2 {
                    return Err("request/header requires session envelope v2".to_owned());
                }
                header.validate().map_err(|error| error.to_string())
            }
            Self::RequestContext { context, .. } => {
                if version < 2 {
                    return Err("request/context requires session envelope v2".to_owned());
                }
                context.validate().map_err(|error| error.to_string())
            }
            Self::AssistantProviderItem { item, .. } => {
                if version < 2 {
                    return Err("assistant/provider-item requires session envelope v2".to_owned());
                }
                item.validate().map_err(|error| error.to_string())
            }
            Self::AssistantAudio { attachments, .. } => {
                if version < 2 {
                    return Err("assistant/audio requires session envelope v2".to_owned());
                }
                if attachments.is_empty() || attachments.len() > 4 {
                    return Err("assistant/audio count is invalid".to_owned());
                }
                let mut ids = std::collections::BTreeSet::new();
                for attachment in attachments {
                    attachment.validate().map_err(|error| error.to_string())?;
                    if !attachment.media_type().is_audio()
                        || attachment.audio().is_none()
                        || !ids.insert(attachment.content_id().as_str())
                    {
                        return Err("assistant/audio metadata is invalid".to_owned());
                    }
                }
                Ok(())
            }
            Self::AssistantResponseMetadata { metadata, .. } => {
                if version < 2 {
                    return Err(
                        "assistant/response-metadata requires session envelope v2".to_owned()
                    );
                }
                metadata.validate().map_err(|error| error.to_string())
            }
            Self::ServerToolCall { call, .. } => {
                if version < 2 {
                    return Err("server-tool/call requires session envelope v2".to_owned());
                }
                call.validate().map_err(|error| error.to_string())
            }
            Self::ServerToolResult { result, .. } => {
                if version < 2 {
                    return Err("server-tool/result requires session envelope v2".to_owned());
                }
                result.validate().map_err(|error| error.to_string())
            }
            Self::ServerToolUsage { usage, .. } => {
                if version < 2 {
                    return Err("server-tool/usage requires session envelope v2".to_owned());
                }
                usage.validate().map_err(|error| error.to_string())
            }
            Self::AssistantCitation { citation, .. } => {
                if version < 2 {
                    return Err("assistant/citation requires session envelope v2".to_owned());
                }
                citation.validate().map_err(|error| error.to_string())
            }
            Self::AttachmentAdded { attachment } => {
                if version < 2 {
                    return Err("attachment/added requires session envelope v2".to_owned());
                }
                attachment.validate().map_err(|error| error.to_string())
            }
            Self::UserAttachments {
                attachments,
                document_routes,
            } => {
                if version < 2 {
                    return Err("user/attachments requires session envelope v2".to_owned());
                }
                if attachments.is_empty() || attachments.len() > 16 {
                    return Err("user/attachments count is invalid".to_owned());
                }
                let mut ids = std::collections::BTreeSet::new();
                for attachment in attachments {
                    attachment.validate().map_err(|error| error.to_string())?;
                    if !ids.insert(attachment.content_id().as_str()) {
                        return Err("user/attachments contains duplicate content".to_owned());
                    }
                }
                validate_document_routes(attachments, document_routes)
            }
            Self::ToolResult {
                untrusted_content: Some(_),
                ..
            } if version < 2 => {
                Err("tool/result untrusted content requires session envelope v2".to_owned())
            }
            Self::RichToolResult { result, .. } => {
                if version < 2 {
                    return Err("tool/rich-result requires session envelope v2".to_owned());
                }
                result.validate().map_err(|error| error.to_string())
            }
            Self::AgentInboxSplice {
                target,
                removed_count,
                inserted,
                outcome,
                ..
            } => {
                if version < 2 {
                    return Err("agent/inbox/splice requires session envelope v2".to_owned());
                }
                crate::inbox::validate_splice_shape(*target, *removed_count, inserted, *outcome)
            }
            Self::GoalChange { change } => {
                if version < 2 {
                    return Err("goal/change requires session envelope v2".to_owned());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::WorkflowChange { change } => {
                if version < 2 {
                    return Err("workflow/change requires session envelope v2".to_owned());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::ScheduleChange { change } => {
                if version < 2 {
                    return Err("schedule/change requires session envelope v2".to_owned());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::WorkChange { change } => {
                if version < 2 {
                    return Err("work/change requires session envelope v2".into());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::TeamChange { change } => {
                if version < 2 {
                    return Err("team/change requires session envelope v2".to_owned());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::ReviewChange { change } => {
                if version < 2 {
                    return Err("review/change requires session envelope v2".to_owned());
                }
                change.validate_shape().map_err(|error| error.to_string())
            }
            Self::CodeModeChange { change } => {
                if version < 2 {
                    return Err("code-mode/change requires session envelope v2".into());
                }
                change.validate_shape()
            }
            Self::HookContribution { contribution } => {
                if version < 2 {
                    return Err("hook/contribution requires session envelope v2".to_owned());
                }
                contribution.validate().map_err(|error| error.to_string())
            }
            Self::CompactionApplied { summary, .. } => {
                if summary.trim().is_empty() || summary.len() > 64 * 1024 {
                    return Err("compaction/applied summary is invalid".to_owned());
                }
                Ok(())
            }
            Self::NativeCompactionApplied {
                strategy, items, ..
            } => {
                if version < 2 {
                    return Err("compaction/native requires session envelope v2".to_owned());
                }
                let valid_strategy = !strategy.is_empty()
                    && strategy.len() <= 64
                    && strategy.split('-').all(|part| {
                        !part.is_empty()
                            && part
                                .bytes()
                                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                    });
                let first = items.first();
                if !valid_strategy
                    || items.len() > 256
                    || first.is_none_or(|item| {
                        item.protocol() == heycode_core::ProviderProtocol::Unknown
                    })
                {
                    return Err("compaction/native checkpoint is invalid".to_owned());
                }
                let Some(first) = first else {
                    return Err("compaction/native checkpoint is invalid".to_owned());
                };
                if items.iter().any(|item| {
                    item.validate().is_err()
                        || item.provider() != first.provider()
                        || item.model() != first.model()
                        || item.protocol() != first.protocol()
                }) {
                    return Err("compaction/native checkpoint is invalid".to_owned());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn valid_runtime_control(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_runtime_tools(tools: &[heycode_core::ToolSpec]) -> bool {
    if tools.len() > 256 {
        return false;
    }
    let mut names = std::collections::BTreeSet::new();
    tools.iter().all(|tool| {
        valid_runtime_control(&tool.name, 128)
            && tool
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            && names.insert(tool.name.as_str())
            && !tool.description.trim().is_empty()
            && tool.description.len() <= 4 * 1024
            && !tool.description.contains('\0')
            && tool.parameters.is_object()
            && serde_json::to_vec(&tool.parameters).is_ok_and(|wire| wire.len() <= 64 * 1024)
    })
}

fn validate_document_routes(
    attachments: &[heycode_core::AttachmentMetadata],
    routes: &[heycode_core::DocumentInputRoute],
) -> Result<(), String> {
    if routes.len() > 4 {
        return Err("user/attachments document route count is invalid".to_owned());
    }
    let mut sources = std::collections::BTreeSet::new();
    let mut selected = std::collections::BTreeSet::new();
    for route in routes {
        route.validate().map_err(|error| error.to_string())?;
        if !sources.insert(route.source().content_id().as_str())
            || !selected.insert(route.selected().content_id().as_str())
            || !attachments
                .iter()
                .any(|attachment| attachment == route.selected())
        {
            return Err("user/attachments document routes are incoherent".to_owned());
        }
    }
    for attachment in attachments {
        let route_count = routes
            .iter()
            .filter(|route| route.selected() == attachment)
            .count();
        if ((attachment.media_type().is_image() || attachment.media_type().is_audio())
            && route_count != 0)
            || (!attachment.media_type().is_image()
                && !attachment.media_type().is_audio()
                && route_count != 1)
        {
            return Err("user/attachments route does not match selected media".to_owned());
        }
    }
    Ok(())
}

/// The value a front end should render for a committed plain tool result.
///
/// `ToolResult.content` is the text the model saw. Built-in tools commit the
/// JSON they returned (an `edit` result is `{"diff": …, "message": …}`), so a
/// content that parses as a JSON object or array is handed back structured —
/// a diff card needs the fields, not a quoted blob — and anything else stays
/// the string it is. Truncated (`…[truncated]`) or prose content never parses
/// and so is never misread.
#[must_use]
pub fn tool_result_value(content: &str) -> serde_json::Value {
    let trimmed = content.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
        && (value.is_object() || value.is_array())
    {
        return value;
    }
    serde_json::Value::String(content.to_owned())
}

/// One envelope line of the session log: `{v, seq, time_ms, kind, data}`.
///
/// Serialize-only by design; readers reconstruct kinds through the validated
/// head path in [`crate::Session::open`] rather than a flattened deserialize.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionEvent {
    /// Source envelope format version (1 for migrated legacy lines, 2 for new lines).
    pub v: u8,
    /// Zero-based position in the log; contiguous by construction.
    pub seq: u64,
    /// Wall-clock commit time in milliseconds since the Unix epoch.
    pub time_ms: i64,
    /// Kind-specific payload, flattened onto the line.
    #[serde(flatten)]
    pub kind: SessionEventKind,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn committed_json_tool_results_come_back_structured_and_prose_stays_text() {
        assert_eq!(
            tool_result_value("{\"diff\": \"-a\\n+b\", \"message\": \"ok\"}"),
            json!({"diff": "-a\n+b", "message": "ok"})
        );
        assert_eq!(tool_result_value("[1, 2]"), json!([1, 2]));
        assert_eq!(tool_result_value("plain text"), json!("plain text"));
        assert_eq!(
            tool_result_value("{\"diff\": \"…[truncated]"),
            json!("{\"diff\": \"…[truncated]"),
            "a truncated blob is not JSON and must not be mis-parsed"
        );
        assert_eq!(tool_result_value("42"), json!("42"), "scalars stay text");
    }

    fn sample(seq: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms: 1_730_000_000_000_i64,
            kind,
        }
    }

    fn attachment() -> heycode_core::AttachmentMetadata {
        heycode_core::AttachmentMetadata::new(
            heycode_core::AttachmentContentId::from_sha256([0x44; 32]),
            heycode_core::AttachmentMediaType::new("image/png").unwrap(),
            100,
            Some("image.png".to_owned()),
            Some(heycode_core::AttachmentDimensions::new(10, 10).unwrap()),
        )
        .unwrap()
    }

    fn audio_attachment() -> heycode_core::AttachmentMetadata {
        heycode_core::AttachmentMetadata::new_audio(
            heycode_core::AttachmentContentId::from_sha256([0x45; 32]),
            heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
            16_044,
            Some("answer.wav".to_owned()),
            heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn kind_tags_match_wire_names() {
        let cases: Vec<(SessionEventKind, &'static str)> = vec![
            (
                SessionEventKind::SessionCreated {
                    creation: Box::new(
                        crate::SessionCreation::new(
                            crate::SessionCreationMetadata::new(
                                Some(std::path::PathBuf::from("/work/project")),
                                Some("native".to_owned()),
                                crate::SessionSource::Interactive,
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    ),
                },
                "session/created",
            ),
            (
                SessionEventKind::RuntimeLinked {
                    runtime: "codex".to_owned(),
                    runtime_session_id: "0199c3d0-1c3f-7a11-9f31-52d4b1f0aa10".to_owned(),
                },
                "runtime/linked",
            ),
            (
                SessionEventKind::RuntimeConfigured {
                    state: RuntimeConfigurationState::Committed,
                    system_prompt: Some("instructions".to_owned()),
                    tools: Some(vec![heycode_core::ToolSpec {
                        name: "read".to_owned(),
                        description: "Read a file".to_owned(),
                        parameters: json!({"type":"object"}),
                    }]),
                    model: Some("model".to_owned()),
                    reasoning_effort: Some("high".to_owned()),
                },
                "runtime/configured",
            ),
            (SessionEventKind::TurnStart { turn: 0 }, "turn/start"),
            (
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
                "turn/end",
            ),
            (
                SessionEventKind::StepStart { turn: 0, step: 1 },
                "step/start",
            ),
            (SessionEventKind::StepEnd { turn: 0, step: 1 }, "step/end"),
            (
                SessionEventKind::AgentInboxSplice {
                    target: crate::InboxTarget::NextTurn,
                    start: 0,
                    removed_count: None,
                    inserted: vec![
                        crate::InboxMessage::with_id(
                            crate::InboxMessageId::new("inbox_1").unwrap(),
                            crate::InboxDelivery::FollowUp,
                            "continue",
                        )
                        .unwrap(),
                    ],
                    outcome: None,
                },
                "agent/inbox/splice",
            ),
            (
                SessionEventKind::GoalChange {
                    change: Box::new(crate::GoalChange::snapshot(
                        crate::GoalOperation::Create,
                        crate::GoalSnapshot::new(
                            crate::GoalId::new("goal_1").unwrap(),
                            1,
                            "finish the task",
                            crate::GoalPhase::Active,
                            None,
                            8,
                        )
                        .unwrap(),
                        0,
                        1,
                        1,
                    )),
                },
                "goal/change",
            ),
            (
                SessionEventKind::WorkflowChange {
                    change: Box::new(crate::WorkflowChange::start(
                        crate::WorkflowRunId::new("workflow_1").unwrap(),
                        crate::WorkflowDefinition::new(
                            "verify",
                            "verify the task",
                            vec![crate::WorkflowCapability::Progress],
                            vec![
                                crate::WorkflowStep::new(
                                    "step_1",
                                    "verify",
                                    crate::WorkflowAction::Emit {
                                        value: serde_json::json!({"ok":true}),
                                    },
                                )
                                .unwrap(),
                            ],
                        )
                        .unwrap(),
                    )),
                },
                "workflow/change",
            ),
            (
                SessionEventKind::ScheduleChange {
                    change: Box::new(crate::ScheduleChange::create(
                        crate::ScheduleRecord::at(
                            crate::ScheduleId::new("schedule_1").unwrap(),
                            "verify later",
                            1_730_000_001_000,
                        )
                        .unwrap(),
                    )),
                },
                "schedule/change",
            ),
            (
                SessionEventKind::WorkChange {
                    change: Box::new(
                        crate::WorkChange::create(
                            crate::WorkScope::Session,
                            "wire-work",
                            crate::WorkItemFields {
                                subject: "Verify work".into(),
                                description: String::new(),
                                status: crate::WorkStatus::Pending,
                                owner: None,
                                dependencies: vec![],
                                metadata: Default::default(),
                            },
                        )
                        .unwrap(),
                    ),
                },
                "work/change",
            ),
            (
                SessionEventKind::TeamChange {
                    change: Box::new(
                        crate::TeamChange::created(
                            crate::TeamId::new("team_1").unwrap(),
                            crate::TeamMember::new(
                                crate::TeamMemberId::new("lead").unwrap(),
                                "Lead",
                                crate::TeamRole::Lead,
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    ),
                },
                "team/change",
            ),
            (
                SessionEventKind::ReviewChange {
                    change: Box::new(
                        crate::ReviewChange::started(
                            crate::ReviewRunId::new("review_1").unwrap(),
                            "codex",
                            "0123456789012345678901234567890123456789",
                            "",
                            "review",
                        )
                        .unwrap(),
                    ),
                },
                "review/change",
            ),
            (
                SessionEventKind::CodeModeChange {
                    change: Box::new(crate::CodeModeChange::Saved {
                        script: crate::SavedScript {
                            name: "fixture".into(),
                            source: "return 1;".into(),
                            tools: Default::default(),
                        },
                    }),
                },
                "code-mode/change",
            ),
            (
                SessionEventKind::HookContribution {
                    contribution: Box::new(
                        crate::HookContributionRecord::new(
                            "fixture-owner",
                            crate::HookContributionPhase::Pre,
                            crate::HookContributionEvent::UserPrompt,
                            crate::HookContributionHandler::Prompt,
                            None,
                            "hook context",
                        )
                        .unwrap(),
                    ),
                },
                "hook/contribution",
            ),
            (
                SessionEventKind::RequestHeader {
                    turn: 0,
                    step: 1,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    header: Box::new(
                        crate::RequestHeaderSnapshot::new(
                            "provider",
                            "model",
                            heycode_core::ProviderProtocol::OpenAiChatCompletions,
                            crate::RequestTargetSnapshot::Http {
                                base_url: "https://example.test/v1".to_owned(),
                            },
                            crate::RequestAuthenticationSnapshot::None,
                            Some("system".to_owned()),
                            Vec::new(),
                            crate::RequestOptionsSnapshot {
                                input_modalities: vec!["text".to_owned()],
                                reasoning_effort: None,
                                defaulted_reasoning_effort: false,
                                structured_output: None,
                                native_features: Vec::new(),
                                native_tool_routes: Vec::new(),
                                provider_options: Vec::new(),
                                temperature: None,
                                max_output_tokens: None,
                                defaulted_max_output_tokens: false,
                                purpose: "conversation".to_owned(),
                                retry: None,
                            },
                        )
                        .unwrap(),
                    ),
                },
                "request/header",
            ),
            (
                SessionEventKind::RequestContext {
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    context: crate::RequestContextSnapshot::new(Some(100), Some(10), None, None, 1)
                        .unwrap(),
                },
                "request/context",
            ),
            (
                SessionEventKind::UserMessage { text: "hi".into() },
                "user/message",
            ),
            (
                SessionEventKind::UserAttachments {
                    attachments: vec![attachment()],
                    document_routes: Vec::new(),
                },
                "user/attachments",
            ),
            (
                SessionEventKind::AttachmentAdded {
                    attachment: Box::new(attachment()),
                },
                "attachment/added",
            ),
            (
                SessionEventKind::AssistantChunk {
                    turn: 0,
                    step: 0,
                    text: None,
                    reasoning: None,
                },
                "assistant/chunk",
            ),
            (
                SessionEventKind::AssistantMessage {
                    turn: 0,
                    step: 0,
                    content: String::new(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                },
                "assistant/message",
            ),
            (
                SessionEventKind::AssistantAudio {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    attachments: vec![audio_attachment()],
                },
                "assistant/audio",
            ),
            (
                SessionEventKind::AssistantProviderItem {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 0,
                    item: Box::new(
                        heycode_core::ProviderStateItem::new(
                            "provider",
                            "model",
                            heycode_core::ProviderProtocol::OpenAiChatCompletions,
                            heycode_core::ProviderStateKind::ChatAssistantMessage,
                            json!({"role":"assistant","content":"hi"}),
                        )
                        .unwrap(),
                    ),
                },
                "assistant/provider-item",
            ),
            (
                SessionEventKind::AssistantResponseMetadata {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    metadata: Box::new(
                        heycode_core::ProviderResponseMetadata::new(
                            Some(heycode_core::ProviderCacheUsage::new(10, 2, 4, 1).unwrap()),
                            Vec::new(),
                            None,
                        )
                        .unwrap(),
                    ),
                },
                "assistant/response-metadata",
            ),
            (
                SessionEventKind::ServerToolCall {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 0,
                    call: Box::new(
                        heycode_core::ServerToolCall::new(
                            heycode_core::CallId::from_raw("srvtoolu_1"),
                            "web_search",
                            "web_search",
                            json!({"query":"rust"}),
                        )
                        .unwrap(),
                    ),
                },
                "server-tool/call",
            ),
            (
                SessionEventKind::ServerToolResult {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 1,
                    result: Box::new(
                        heycode_core::ServerToolResult::success(
                            heycode_core::CallId::from_raw("srvtoolu_1"),
                            Some(0),
                            Vec::new(),
                        )
                        .unwrap(),
                    ),
                },
                "server-tool/result",
            ),
            (
                SessionEventKind::ServerToolUsage {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    usage: Box::new(
                        heycode_core::ServerToolUsage::new(
                            "web_search",
                            1,
                            heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                            heycode_core::ServerToolUsageCost::Unknown,
                        )
                        .unwrap(),
                    ),
                },
                "server-tool/usage",
            ),
            (
                SessionEventKind::AssistantCitation {
                    turn: 0,
                    step: 0,
                    request_id: heycode_core::RequestId::from_raw("req_1"),
                    output_index: 2,
                    citation: Box::new(
                        heycode_core::UrlCitation::new(
                            "https://example.test/rust",
                            Some("Rust"),
                            None,
                            None,
                            None,
                        )
                        .unwrap(),
                    ),
                },
                "assistant/citation",
            ),
            (
                SessionEventKind::ToolCall {
                    turn: 0,
                    call_id: heycode_core::CallId::from_raw("c1"),
                    name: "bash".into(),
                    args: json!({}),
                },
                "tool/call",
            ),
            (
                SessionEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw("c1"),
                    content: "ok".into(),
                    is_error: false,
                    untrusted_content: None,
                },
                "tool/result",
            ),
            (
                SessionEventKind::RichToolResult {
                    call_id: heycode_core::CallId::from_raw("c2"),
                    result: Box::new(
                        heycode_core::DurableToolResult::new(
                            vec![heycode_core::DurableToolResultBlock::Text {
                                text: "rich".into(),
                                metadata: heycode_core::ToolResultBlockMetadata::default(),
                            }],
                            heycode_core::ToolStructuredContent::Absent,
                            heycode_core::ToolResultSchemaCheck::NoSchema,
                            serde_json::Map::new(),
                        )
                        .unwrap(),
                    ),
                    is_error: false,
                    untrusted_content: Some(heycode_core::UntrustedContentBoundary::mcp()),
                },
                "tool/rich-result",
            ),
            (SessionEventKind::SessionActivated {}, "session/activated"),
            (SessionEventKind::PlanMode { active: true }, "plan/mode"),
            (
                SessionEventKind::PlanReview {
                    plan: "# Full plan".into(),
                    decision: "pending".into(),
                    feedback: String::new(),
                },
                "plan/review",
            ),
            (
                SessionEventKind::SessionTitle { title: "t".into() },
                "session/title",
            ),
            (
                SessionEventKind::CompactionApplied {
                    summary: "s".into(),
                    replaced_upto_seq: 3,
                },
                "compaction/applied",
            ),
            (
                SessionEventKind::NativeCompactionApplied {
                    strategy: "provider-native".to_owned(),
                    replaced_upto_seq: 3,
                    items: vec![
                        heycode_core::ProviderStateItem::new(
                            "openai",
                            "gpt-5.6",
                            heycode_core::ProviderProtocol::OpenAiResponses,
                            heycode_core::ProviderStateKind::ResponseOutputItem,
                            serde_json::json!({
                                "type":"compaction",
                                "id":"cmp_1",
                                "encrypted_content":"opaque"
                            }),
                        )
                        .unwrap(),
                    ],
                    usage: None,
                },
                "compaction/native",
            ),
        ];
        assert_eq!(
            cases.len(),
            KNOWN_KINDS_V2.len(),
            "every variant must be covered"
        );
        for (kind, tag) in cases {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(value["kind"].as_str().unwrap(), tag);
            assert_eq!(kind.name(), tag, "name() must equal the serialized tag");
            assert!(
                KNOWN_KINDS_V2.contains(&tag),
                "{tag} must be in the known set"
            );
        }
    }

    #[test]
    fn runtime_configuration_preserves_explicitly_empty_tools() {
        let unspecified = SessionEventKind::RuntimeConfigured {
            state: RuntimeConfigurationState::Committed,
            system_prompt: None,
            tools: None,
            model: Some("model".to_owned()),
            reasoning_effort: None,
        };
        let disabled = SessionEventKind::RuntimeConfigured {
            state: RuntimeConfigurationState::Committed,
            system_prompt: None,
            tools: Some(Vec::new()),
            model: Some("model".to_owned()),
            reasoning_effort: None,
        };
        let unspecified_wire = serde_json::to_value(&unspecified).unwrap();
        let disabled_wire = serde_json::to_value(&disabled).unwrap();
        assert!(unspecified_wire["data"].get("tools").is_none());
        assert_eq!(disabled_wire["data"]["tools"], json!([]));
        assert_eq!(
            serde_json::from_value::<SessionEventKind>(disabled_wire).unwrap(),
            disabled
        );
    }

    #[test]
    fn runtime_configuration_state_is_backward_compatible_and_audits_attempts() {
        let legacy = json!({
            "kind":"runtime/configured",
            "data":{"model":"legacy-model"}
        });
        let decoded = serde_json::from_value::<SessionEventKind>(legacy).unwrap();
        assert!(matches!(
            decoded,
            SessionEventKind::RuntimeConfigured {
                state: RuntimeConfigurationState::Committed,
                ..
            }
        ));

        let attempted = SessionEventKind::RuntimeConfigured {
            state: RuntimeConfigurationState::Attempted,
            system_prompt: Some("exact input".to_owned()),
            tools: Some(Vec::new()),
            model: Some("model".to_owned()),
            reasoning_effort: Some("high".to_owned()),
        };
        let wire = serde_json::to_value(attempted).unwrap();
        assert_eq!(wire["data"]["state"], "attempted");
        assert_eq!(wire["data"]["system_prompt"], "exact input");
        assert_eq!(wire["data"]["tools"], json!([]));
    }

    #[test]
    fn turn_end_reason_serializes_snake_case() {
        for (reason, expected) in [
            (TurnEndReason::MaxTokens, "max_tokens"),
            (TurnEndReason::MaxSteps, "max_steps"),
            (TurnEndReason::MaxElapsed, "max_elapsed"),
            (TurnEndReason::MaxToolCalls, "max_tool_calls"),
            (
                TurnEndReason::UnreportedTokenUsage,
                "unreported_token_usage",
            ),
            (TurnEndReason::ClockUnavailable, "clock_unavailable"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(expected));
        }
    }

    #[test]
    fn envelope_flattens_kind_and_data() {
        let line = serde_json::to_value(sample(
            7,
            SessionEventKind::UserMessage { text: "hi".into() },
        ))
        .unwrap();
        assert_eq!(
            line,
            json!({
                "v": CURRENT_SESSION_LOG_VERSION,
                "seq": 7,
                "time_ms": 1_730_000_000_000_i64,
                "kind": "user/message",
                "data": { "text": "hi" }
            })
        );
    }

    #[test]
    fn optional_fields_are_omitted_when_none() {
        let value = serde_json::to_value(sample(
            0,
            SessionEventKind::AssistantChunk {
                turn: 2,
                step: 3,
                text: None,
                reasoning: Some("thinking".into()),
            },
        ))
        .unwrap();
        let data = value["data"].as_object().unwrap();
        assert_eq!(
            data.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["reasoning", "step", "turn"]
        );
    }

    #[test]
    fn complex_kinds_round_trip_through_value() {
        let kinds = vec![
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 2,
                content: String::new(),
                reasoning: Some("why".into()),
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: r#"{"cmd":"ls"}"#.into(),
                }]),
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                }),
            },
            SessionEventKind::ToolCall {
                turn: 1,
                call_id: heycode_core::CallId::from_raw("call_1"),
                name: "bash".into(),
                args: json!({"cmd": "ls", "nested": {"a": [1, 2]}}),
            },
            SessionEventKind::CompactionApplied {
                summary: "sum".into(),
                replaced_upto_seq: 9,
            },
            SessionEventKind::NativeCompactionApplied {
                strategy: "provider-native".to_owned(),
                replaced_upto_seq: 9,
                items: vec![
                    heycode_core::ProviderStateItem::new(
                        "openai",
                        "gpt-5.6",
                        heycode_core::ProviderProtocol::OpenAiResponses,
                        heycode_core::ProviderStateKind::ResponseOutputItem,
                        serde_json::json!({
                            "type":"compaction",
                            "id":"cmp_1",
                            "encrypted_content":"opaque"
                        }),
                    )
                    .unwrap(),
                ],
                usage: Some(TokenUsage {
                    prompt_tokens: 9,
                    completion_tokens: 2,
                }),
            },
        ];
        for kind in kinds {
            let value: Value = serde_json::to_value(&kind).unwrap();
            let back: SessionEventKind = serde_json::from_value(value).unwrap();
            assert_eq!(back, kind);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod call_id_newtype_tests {
    use super::*;

    /// Principle #10: opaque ids are newtypes across boundaries. Adopting
    /// `CallId` must NOT change the wire format — v1 logs on disk stay
    /// readable and byte-identical.
    #[test]
    fn call_id_is_transparent_on_the_wire() {
        let kind = SessionEventKind::ToolCall {
            turn: 1,
            call_id: heycode_core::CallId::from_raw("call_9"),
            name: "read".to_owned(),
            args: serde_json::json!({"path": "a.txt"}),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(
            json["data"]["call_id"],
            serde_json::Value::String("call_9".to_owned()),
            "call_id must serialize as a bare string, not a wrapper object"
        );

        let back: SessionEventKind = serde_json::from_value(json).unwrap();
        match back {
            SessionEventKind::ToolCall { call_id, .. } => {
                assert_eq!(call_id.as_str(), "call_9");
            }
            other => panic!("round-trip changed the kind: {other:?}"),
        }
    }
}
