//! heycode-agent — the turn/step loop and the human-facing planes around it.
//!
//! The [`Agent`] drives turns: claim input → step (request → stream → tools)
//! → repeat until nothing is owed. Every model-visible fact is appended to the
//! session log BEFORE it is announced on the UI bus (publish at commit point).
//! Live progress travels as [`UiEvent`]s; durable truth lives in
//! `SessionEvent`s and is what any replay must rebuild from.

mod advisor;
mod agent;
mod approval;
mod attachments;
mod cancellation;
mod code_mode;
mod command_descriptor;
mod commands;
mod script_controls;
mod session_control;
pub use session_control::{AsyncQuestion, OutputStyle, RewindPoint};
mod compact;
mod compaction_registry;
mod deferred_tools;
mod documents;
mod durable_schedule;
mod execution_foreground;
mod execution_jobs;
mod execution_monitor;
mod execution_output;
mod execution_tools;
mod fallback;
mod goal;
mod inbox;
mod interactive_approval;
mod interactive_question;
mod jobs;
mod lifecycle_hooks;
mod loop_budget;
pub mod plan;
mod plan_entry_tool;
pub mod subagent;
pub mod subagent_provider;
pub mod title;
pub use interactive_approval::{
    AskAnswer, AskNotification, AskSubscription, InteractiveApproval, MAX_DENY_REASON_CHARS,
    PendingAsk, SessionAllowRule,
};
pub use interactive_question::{
    InteractiveQuestion, InteractiveQuestionError, QuestionAnswer, QuestionChoice, QuestionMode,
    QuestionNotification, QuestionSpec, QuestionSubscription,
};
mod mapping;
mod native_runtime;
mod plugin;
mod provider_consumers;
mod provider_switch;
mod request_invariant;
mod review;
mod runtime_subagent;
mod schedule;
mod seams;
mod team;
mod telemetry_metrics;
mod tool_execution_events;
mod work;
pub use tool_execution_events::{ToolExecutionEvent, ToolExecutionPhase};
pub mod testing;
pub mod ui;
mod workflow;
mod workflow_intent;
pub use workflow_intent::workflow_requested;
mod workflow_view;
pub mod workspace_transition;
mod worktree;
mod worktree_snapshot;
mod worktree_subagent;

pub use advisor::{
    ADVISOR_SETTINGS_NAMESPACE, AdvisorError, AdvisorRouteCatalog, AdvisorRouteChoice,
    AdvisorSelection, AdvisorService, AdvisorStatus, SERVICE_ADVISOR, advisor_plugin,
    advisor_settings_definition, parse_advisor_owner,
};
pub use agent::{
    Agent, AgentIdle, CompactionPolicy, InboxWakeBarrier, RecompositionPermit, ToolCatalogRow,
    ToolCatalogSnapshot, TurnReport,
};
pub use approval::{
    AcceptedEdits, AlwaysAsk, ApprovalPolicy, ApprovalPolicyKind, AutoApprove, DenyAll,
    SwitchableApproval, UNPROMPTED_DENY_REASON, UnpromptedDeny, decide_named_tool,
};
pub use attachments::agent_attachments_plugin;
pub use cancellation::AgentCancellation;
pub use code_mode::code_mode_plugin;
pub use command_descriptor::{
    CommandArgument, CommandAvailability, CommandCatalogEntry, CommandDescriptor,
    CommandMetadataError, CommandSource, CommandTiming,
};
pub use commands::{
    Command, CommandRegistration, CommandRegistry, CommandRegistryError, CommandUsageError,
    parse_slash,
};
pub use compact::{AutoCompactionControl, autocompact_plugin};
pub use compaction_registry::{
    CompactionContext, CompactionError, CompactionKind, CompactionOutcome, CompactionPlan,
    CompactionRegistry, CompactionReplacement, CompactionStrategy, CompactionStrategyDescriptor,
    CompactionStrategyId, NativeCompaction, PRUNE_MARKER, PortableCompaction, PruneCompaction,
    compactions_plugin,
};
pub use deferred_tools::{
    CodeModeCall, CodeModeSchedule, DeferredToolCatalog, DeferredToolEntry, DeferredToolError,
    DeferredToolKind, DeferredToolMetrics, DeferredToolPlan, DeferredToolProvider,
    DeferredToolProviderId, DeferredToolRegistration, DeferredToolRequest, DeferredToolSelection,
    LexicalDeferredToolProvider, deferred_tools_plugin,
};
pub use documents::agent_documents_plugin;
pub use durable_schedule::{
    DurableScheduleError, DurableScheduleService, durable_schedules_plugin,
};
pub use execution_jobs::{
    ExecutionJobConfig, ExecutionJobError, ExecutionJobService, execution_jobs_plugin,
    execution_jobs_plugin_with_config,
};
pub use execution_monitor::MonitorConfig;
pub use execution_output::{ExecutionMetadata, ExecutionOutput, OutputPage};
pub use fallback::{FallbackRegistration, RequestFallback};
pub use goal::{
    GoalActivation, GoalError, GoalErrorCode, GoalLiveView, GoalPolicy, GoalService, goal_plugin,
};
pub use inbox::{FollowUpError, InboxPending, InboxWake};
pub use jobs::{
    JobError, JobId, JobOutcome, JobRegistry, JobSettlement, JobSnapshot, JobState, WakeDecision,
};
pub use lifecycle_hooks::{
    LifecycleHookAttachmentError, LifecycleHookDecision, LifecycleHookEvent, LifecycleHookPhase,
    LifecycleHookPort, LifecycleHookReport, LifecycleHookRequest,
};
pub use loop_budget::{
    LOOP_BUDGET_SETTINGS_NAMESPACE, LoopBudgetError, LoopBudgetLayer, LoopBudgetPolicy,
    LoopBudgetState, LoopClock, LoopStopReason, LoopUnknownUsagePolicy, SystemLoopClock,
    loop_budget_plugin, loop_budget_settings_definition, settings_loop_budget_plugin,
};
pub use native_runtime::{NativeAgentRuntime, native_runtime_plugin};
pub use plan::{
    PlanHandle, PlanMode, PlanReviewDecision, PlanReviewRecord, PlanSelection, plan_plugin,
};
pub use plan_entry_tool::EnterPlanModeTool;
pub use plugin::AgentOptions;
pub use plugin::{
    ApprovalSwitchHandle, agent_options_plugin, agent_plugin, approval_plugin, commands_plugin,
    interactive_approval_plugin, interactive_approval_plugin_with_policy,
    switchable_approval_plugin,
};
pub use provider_consumers::provider_telemetry_plugin;
pub use provider_switch::{OpaqueStateResolution, ProviderSwitchError, ProviderSwitchPreparation};
pub use request_invariant::{
    RequestDesyncError, VerifiedExperimentalAudioCall, VerifiedResolvedCall,
    snapshots_from_experimental_audio_call, snapshots_from_resolved_call,
    verify_experimental_audio_call, verify_resolved_call,
};
pub use review::{
    ReviewError, ReviewErrorCode, ReviewPluginConfig, ReviewRequest, ReviewResult, ReviewService,
    review_plugin,
};
pub use runtime_subagent::{
    RuntimeSubagentConfig, RuntimeSubagentProvider, runtime_subagent_plugin,
};
pub use seams::{
    PreStepDecision, RequestDecision, RequestErrorDecision, RequestErrorStage, RequestVerdict,
    StepVerdict,
};
pub use subagent::{NativeSubagentProvider, subagent_jobs_plugin, subagent_plugin};
pub use subagent_provider::{
    DelegationGate, SubagentAuthority, SubagentCapabilities, SubagentContinuation, SubagentError,
    SubagentErrorCode, SubagentHandle, SubagentId, SubagentMessageReceipt, SubagentMetadataError,
    SubagentPreset, SubagentPresetId, SubagentPresetRegistration, SubagentProvider,
    SubagentProviderDescriptor, SubagentProviderId, SubagentProviderRegistration, SubagentRegistry,
    SubagentRequest, SubagentSeed, SubagentStarted,
};
pub use team::{
    TeamBootstrapRole, TeamError, TeamErrorCode, TeamService, TeamTool, render_team, team_plugin,
};
pub use telemetry_metrics::telemetry_metrics_plugin;
pub use ui::{AttachmentComposerAction, BackendControlOwner, UiEvent, UiPanelId, UiPanelIdError};
pub use workflow::{
    NativeWorkflowWorker, SequentialWorkflowWorker, WorkflowError, WorkflowReporter,
    WorkflowService, WorkflowStarted, WorkflowWorker, WorkflowWorkerRequest, WorkflowWorkerResult,
    native_workflow_plugin, workflow_definition_schema, workflow_plugin,
};
pub use worktree::{
    GitCommitId, GitWorktreeLease, GitWorktreeManager, WorktreeError, WorktreeErrorCode,
    WorktreeId, WorktreeOutcome, WorktreeRecovery, WorktreeRetention, WorktreeSnapshot,
};
pub use worktree_subagent::{
    WorktreeRuntimeSubagentProvider, WorktreeSubagentConfig, worktree_subagent_plugin,
};

/// Tool-approval policy service.
pub const SERVICE_APPROVAL: heycode_core::ServiceKey = heycode_core::ServiceKey::new("approval");
/// Interactive approval answer handle, present only in ask mode.
pub const SERVICE_APPROVAL_INTERACTIVE: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("approval-interactive");
/// Live approval-mode switch, present on surfaces that can prompt.
pub const SERVICE_APPROVAL_SWITCH: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("approval-switch");
/// Host-owned model-to-human question broker.
pub const SERVICE_QUESTIONS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("questions");
/// Human-only slash command registry.
pub const SERVICE_COMMANDS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("commands");
/// Resolved agent runtime options.
pub const SERVICE_AGENT_OPTIONS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("agent-options");
/// Native agent-loop handle.
pub const SERVICE_AGENT: heycode_core::ServiceKey = heycode_core::ServiceKey::new("agent");
/// Effect-owned native/portable/prune compaction strategy registry.
pub const SERVICE_COMPACTIONS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("compactions");
/// Durable plan-mode state and guard handle.
pub const SERVICE_PLAN: heycode_core::ServiceKey = heycode_core::ServiceKey::new("plan");
/// Delegation providers plus live continuable children.
pub const SERVICE_SUBAGENTS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("subagents");
/// Background jobs with settlement notices and a bounded wake budget.
pub const SERVICE_JOBS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("jobs");
/// Shell and terminal producers bound to the effect-owned job registry.
pub const SERVICE_EXECUTION_JOBS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("execution-jobs");
/// Revisioned same-session goal domain and bounded round driver.
pub const SERVICE_GOALS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("goals");
/// Checkpointed workflow engine over a replaceable worker Provider.
pub const SERVICE_WORKFLOWS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("workflows");
/// Durable session-local schedule timers and management tools.
pub const SERVICE_SCHEDULES: heycode_core::ServiceKey = heycode_core::ServiceKey::new("schedules");
/// Authority-scoped durable team roster, task DAG and peer mailbox.
pub const SERVICE_TEAMS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("teams");
/// Selectable isolated reviewer runtime and structured result owner.
pub const SERVICE_REVIEWS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("reviews");

mod task_inventory;
pub use task_inventory::{TaskDiagnostic, TaskSnapshot, TaskState};

pub use jobs::JobLimits;

mod subagent_budget;
pub use subagent_budget::{SubagentBudgetLimits, SubagentBudgetSnapshot};
mod subagent_config;
pub use subagent_config::{
    ChildIsolation, ChildMemory, ChildPermissions, ResolvedSubagentConfig, SubagentConfig,
};

mod agent_memory;

mod builtin_presets;
pub use builtin_presets::builtin_native_presets;

pub use plugin::agent_plugin_with_job_limits;
pub use subagent::subagent_plugin_with_budget;

pub use workflow_view::{
    WorkflowAgentView, WorkflowControls, WorkflowCounts, WorkflowJobView, WorkflowNodeKind,
    WorkflowNodeView, WorkflowPhaseView, WorkflowTiming, WorkflowUsage, WorkflowView,
    WorkflowViewState, project_workflow_views,
};

pub use work::{SERVICE_WORK, WorkService, WorkServiceError, work_plugin, work_plugin_with_teams};
