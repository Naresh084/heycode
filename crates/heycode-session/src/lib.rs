//! heycode-session — the durable append-only session log.
//!
//! Every model-visible fact lands here as one [`SessionEvent`] line in
//! `<sessions-root>/<session-id>/session.jsonl`. [`Session`] owns appends,
//! resume, and post-commit emission on the shared `heycode_core::EventBus`.
//! [`derive_messages`] folds the log into neutral provider-ready
//! [`WireMessage`]s owned here. [`project_inbox`] independently rebuilds
//! pending follow-up, steering and injected input plus its settlement ledger;
//! [`SessionQueryService`] lists and owns human-plane lifecycle operations
//! through a replaceable backend, while JSONL remains truth. [`project_repair`] reads what a killed process
//! left open and names each missing outcome unknown or interrupted, never
//! completed; it writes nothing, because the absence of a closing record is
//! the log's own account of the crash. `heycode-llm` never imports session types.

/// Local whole-session terminal ownership and control protocol.
#[cfg(unix)]
pub mod background;
pub mod checkpoints;
mod code_mode;
mod compaction_switch;
mod context_growth;
mod creation;
mod event;
mod hook;
mod inbox;
mod index;
mod lifecycle;
mod orchestration;
mod plugin;
mod projection;
mod query;
mod repair;
mod request;
mod request_configuration;
mod request_projection;
mod review;
mod session;
mod stats;
mod team;
mod usage_projection;
mod work;
mod workflow_observation;

/// Durable session-log service.
pub const SERVICE_SESSION: heycode_core::ServiceKey = heycode_core::ServiceKey::new("session");
/// Replaceable session query and lifecycle service.
pub const SERVICE_SESSION_QUERY: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("session-query");

pub use code_mode::{
    CodeModeChange, CodeModeProjection, SavedScript, ScriptCallView, ScriptRunView,
    project_code_mode,
};
pub use compaction_switch::{OpaqueCompactionBarrier, opaque_compaction_barrier};
pub use context_growth::{RetainedContextGrowth, retained_context_growth};
pub use creation::{
    SessionCreation, SessionCreationMetadata, SessionMetadataError, SessionParent, SessionSource,
};
pub use event::{
    CURRENT_SESSION_LOG_VERSION, MIN_SESSION_LOG_VERSION, RuntimeConfigurationState, SessionEvent,
    SessionEventKind, TokenUsage, ToolCallOut, TurnEndReason, tool_result_value,
};
pub use hook::{
    HookContributionError, HookContributionEvent, HookContributionHandler, HookContributionPhase,
    HookContributionRecord, MAX_HOOK_CONTRIBUTION_BYTES,
};
pub use inbox::{
    AgentCompletionOutcome, InboxDelivery, InboxMessage, InboxMessageError, InboxMessageId,
    InboxProjection, InboxProjectionError, InboxSettlement, InboxSource, InboxSpliceOutcome,
    InboxTarget, project_inbox,
};
pub use index::{
    SessionIndexComparison, SessionIndexError, SessionIndexIssue, SessionIndexSnapshot,
    SqliteSessionIndex,
};
pub use lifecycle::{
    SessionArchiveAction, SessionCreateRequest, SessionDeleteReceipt, SessionDeleteRequest,
    SessionExportFormat, SessionExportReceipt, SessionLineageFilter, SessionStorageFilter,
    SessionStorageState, SessionTitle, discard_unused_session, is_unused_session,
};
pub use orchestration::{
    GoalBlockReason, GoalChange, GoalId, GoalOperation, GoalPhase, GoalProjection,
    GoalProjectionError, GoalRef, GoalSnapshot, GoalView, MAX_ACTIVE_SCHEDULES, ScheduleChange,
    ScheduleId, ScheduleProjection, ScheduleProjectionError, ScheduleRecord, ScheduleRule,
    ScheduleTimeZone, WorkflowAction, WorkflowCapability, WorkflowChange, WorkflowDefinition,
    WorkflowNodeRecord, WorkflowNodeState, WorkflowOutcome, WorkflowProjection,
    WorkflowProjectionError, WorkflowRunId, WorkflowRunProjection, WorkflowState, WorkflowStep,
    project_goal, project_schedules, project_workflows,
};
pub use plugin::{
    session_plugin, session_query_jsonl_plugin, session_resume_plugin,
    session_with_id_and_metadata_plugin, session_with_metadata_plugin,
};
pub use projection::{Role, WireMessage, WireToolCall, derive_messages, derive_messages_repaired};
pub use query::{
    SessionActivityStatus, SessionCursor, SessionFilter, SessionPage, SessionQuery,
    SessionQueryBackend, SessionQueryError, SessionQueryService, SessionSummary,
};
pub use repair::{OpenOutcome, OpenRecord, OpenRecordKind, SessionRepair, project_repair};
pub use request::{
    RequestAuthenticationSnapshot, RequestContextSnapshot, RequestContributorMeasurement,
    RequestContributorSnapshot, RequestHeaderSnapshot, RequestOptionsSnapshot,
    RequestRetrySafetySnapshot, RequestRetrySnapshot, RequestTargetSnapshot, SnapshotError,
};
pub use request_configuration::{RequestConfigurationChange, RequestConfigurationSnapshot};
pub use request_projection::{
    ProjectedInput, ProjectedRequest, ProjectedServerToolEvent, ProjectionError,
    project_inputs_for_route, project_requests,
};
pub use review::{
    FindingOutcome, FindingReport, FindingReportId, FindingReportSource,
    FindingVerificationVerdict, ReportedFinding, ReviewChange, ReviewFailureReason, ReviewFinding,
    ReviewLevel, ReviewMetadataError, ReviewProjection, ReviewProjectionError, ReviewRunId,
    ReviewRunView, ReviewSeverity, ReviewState, project_reviews,
};
pub use session::{
    AppendError, CreateError, ForkBoundary, ForkError, OpenError, Session,
    SessionDowngradeGuidance, SessionVersionGuidanceError,
};
pub use stats::{SessionModelStats, SessionStatsDay, SessionStatsSnapshot};
pub use team::{
    TeamChange, TeamId, TeamLifecycle, TeamMailboxMessage, TeamMember, TeamMemberId, TeamMessageId,
    TeamMetadataError, TeamProjection, TeamProjectionError, TeamRole, TeamTask, TeamTaskId,
    TeamTaskState, TeamView, project_teams,
};
pub use usage_projection::{
    SessionUsage, ToolUsage, ToolUsageSource, TurnOutcome, TurnUsage, project_usage,
};

pub use workflow_observation::{
    WorkflowAgentRecord, WorkflowAgentState, WorkflowJobRecord, WorkflowPhase, WorkflowUsage,
};

pub use work::{
    WorkChange, WorkError, WorkItem, WorkItemFields, WorkItemId, WorkProjection, WorkScope,
    WorkStatus, project_work_items, team_work_id,
};
