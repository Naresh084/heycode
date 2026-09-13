//! Application state and the terminal event loop.

pub mod accessibility;
#[cfg(test)]
mod agent_history_tests;
#[cfg(test)]
mod agent_receipts_tests;
mod inbox_transcript;
pub(crate) mod optional_questions;
#[cfg(unix)]
pub(crate) mod session_background;
mod session_lifecycle;
mod task_navigation;
#[cfg(test)]
mod task_retry_tests;
pub(crate) mod tool_groups;
mod verbose_transcript;
mod workflow_navigation;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use heycode_agent::{BackendControlOwner, CommandRegistry, CommandTiming, UiEvent, parse_slash};
use heycode_core::EventBus;
use heycode_llm::LlmError;

use crate::command_palette::{CommandPaletteMatch, filter_commands};
use crate::command_scheduling::{CommandDisposition, route_command};
use crate::mcp_panel::{
    McpListingSupport, McpPanelIntent, McpPanelKeyOutcome, McpPanelOutcome, McpPanelRow,
    McpPanelView, build_rows, dispatch, health_next_step, health_word,
};
use crate::model_picker::{ModelPickerFilter, ModelPickerMatch, filter_models};
use crate::panel_commands::{
    CapabilityCatalogView, CapabilityPanel, PanelCommandBridge, agents_catalog, hooks_catalog,
};
use crate::permission_picker::{PermissionPickerRow, build_permission_rows};
use crate::plugin_panel::{
    PluginPackageIndex, PluginPanelIntent, PluginPanelKeyOutcome, PluginPanelOutcome,
    PluginPanelRow, PluginPanelView, build_rows as build_plugin_rows, dispatch as dispatch_plugin,
    list as list_plugins,
};
use crate::route_picker::{
    RoutePickerFilter, RoutePickerMatch, RoutePickerRow, RoutePickerSelection, build_route_rows,
    filter_routes,
};
use crate::session_browser::{SessionBrowserAction, SessionBrowserView, SessionCommandRequest};
use crate::settings_panel::{SettingsPanelKeyOutcome, SettingsPanelView, SettingsShellView};
use crate::side_panel::{
    SidePanelKind, SidePanelSnapshot, agents_snapshot, diff_snapshot, jobs_snapshot,
};

/// Presentation and observed timing for one provider-supplied reasoning block.
#[derive(Debug, Clone, Default)]
pub struct ReasoningView {
    /// Routine tool group that owns this completed reasoning block.
    pub group_parent: Option<usize>,
    /// Hide the block while its tool group is collapsed.
    pub group_hidden: bool,
    /// Show the retained reasoning when its owning group is expanded.
    pub group_details: bool,
    /// Per-block choice; absence follows the global preference.
    pub expanded: Option<bool>,
    /// Observed elapsed time; absent for replay without timing evidence.
    pub elapsed_seconds: Option<u64>,
    /// Whether the phase was interrupted rather than completed.
    pub interrupted: bool,
    /// Keyboard focus on this block's header.
    pub focused: bool,
    started: Option<std::time::Instant>,
}

impl ReasoningView {
    fn live() -> Self {
        Self {
            started: Some(std::time::Instant::now()),
            elapsed_seconds: Some(0),
            ..Self::default()
        }
    }

    fn finish(&mut self, interrupted: bool) {
        if let Some(started) = self.started.take() {
            self.elapsed_seconds = Some(started.elapsed().as_secs());
        }
        self.interrupted = interrupted;
    }

    /// Compact header shared by visual and accessible rendering.
    #[must_use]
    pub fn label(&self, done: bool, expanded: bool) -> String {
        let verb = if self.interrupted {
            "Thinking interrupted"
        } else if done {
            "Thought"
        } else {
            "Thinking…"
        };
        let timing = self
            .elapsed_seconds
            .map(|seconds| {
                let duration = if seconds == 0 {
                    "<1s".to_owned()
                } else {
                    format!("{seconds}s")
                };
                if done && !self.interrupted {
                    format!(" for {duration}")
                } else {
                    format!(" ({duration})")
                }
            })
            .unwrap_or_default();
        format!("{} {verb}{timing}", if expanded { "▾" } else { "▸" })
    }
}

/// Local display state for a tool's single, updating transcript card.
#[derive(Debug, Clone, Default)]
pub struct ToolViewState {
    /// Job identity from an admitted, source-attributed completion envelope.
    pub completed_job: Option<String>,
    /// Unique child label correlated by the authoritative task owner's job identity.
    pub completed_agent_label: Option<String>,
    /// Authoritative children admitted by this exact tool occurrence.
    pub spawn_children: Vec<crate::task_console::TaskRecord>,
    /// Combined presentation for adjacent spawn calls; originals retain ownership.
    pub spawn_group_children: Vec<crate::task_console::TaskRecord>,
    /// Human summary on the first call of a consecutive successful tool group.
    pub group_summary: Option<String>,
    /// Owning first call for the other members of a tool group.
    pub group_parent: Option<usize>,
    /// Hide a member while its owning group is collapsed.
    pub group_hidden: bool,
    /// Render full retained details when the owning group is expanded.
    pub group_details: bool,
    /// Show retained arguments and output instead of the compact summary.
    pub expanded: bool,
    /// Last observed approval decision for this exact visible occurrence.
    pub approval: Option<String>,
    /// Keyboard focus on the card header.
    pub focused: bool,
    /// Retrieved output attached to this original call, keyed by stream and byte offset.
    pub retrieved_output: std::collections::BTreeMap<(String, u64), String>,
    /// This retrieval is represented by its original tool card.
    pub merged: bool,
}

impl ToolViewState {
    /// Children shown by this card, including an adjacent spawn group when present.
    #[must_use]
    pub fn spawn_tree(&self) -> &[crate::task_console::TaskRecord] {
        if self.spawn_group_children.is_empty() {
            &self.spawn_children
        } else {
            &self.spawn_group_children
        }
    }
    /// Approval and execution are distinct states on the same card.
    #[must_use]
    pub fn status(&self, result: Option<&(bool, serde_json::Value)>) -> String {
        let execution = result.map(|(ok, _)| if *ok { "completed" } else { "failed" });
        match (self.approval.as_deref(), execution) {
            (Some(approval), Some(execution)) => format!(
                "{} · {execution}",
                approval.split(':').next().unwrap_or(approval)
            ),
            (Some(approval), None) => approval.split(':').next().unwrap_or(approval).into(),
            (None, Some(execution)) => execution.into(),
            (None, None) => "running".into(),
        }
    }
}

/// One rendered transcript row-group.
#[derive(Debug, Clone)]
pub enum Item {
    /// `❯ text` — the user's line.
    User(String),
    /// Ephemeral accepted slash command. It is never a model message or a
    /// durable session event, and its display text omits unreviewed arguments.
    Command(String),
    /// Durable image metadata paired with the following user text.
    Attachments {
        /// Selected image/derived/native document metadata.
        attachments: Vec<heycode_core::AttachmentMetadata>,
        /// Explicit native/extracted route for document rows.
        document_routes: Vec<heycode_core::DocumentInputRoute>,
    },
    /// Assistant audio output committed as durable metadata only.
    AudioOutput {
        /// One to four exact ATT01 audio records.
        attachments: Vec<heycode_core::AttachmentMetadata>,
    },
    /// Streaming/final assistant markdown.
    Assistant(String),
    /// Dim reasoning stream; `done` collapses it to a summary line.
    Reasoning {
        /// Accumulated reasoning text.
        text: String,
        /// Turn finished (block complete).
        done: bool,
        /// Expansion choice and observed phase duration.
        view: ReasoningView,
    },
    /// `⏺ name(args)` + optional structured result for per-tool views.
    Tool {
        /// Durable call identity on replay/session-owned live projection.
        call_id: Option<heycode_core::CallId>,
        /// Tool name.
        name: String,
        /// Full argument object as received from the model.
        args: serde_json::Value,
        /// Settled outcome: (ok, full result value).
        result: Option<(bool, serde_json::Value)>,
        /// External content classification for the settled result.
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
        /// Expansion and approval state; output remains retained when collapsed.
        view: ToolViewState,
    },
    /// Safe identity for one opaque provider continuation item. Its data is
    /// deliberately absent: normalized cards, not provider JSON, are UI input.
    ProviderState {
        /// Owning provider.
        provider: String,
        /// Canonical model.
        model: String,
        /// Stable protocol label.
        protocol: String,
        /// Stable provider-state kind label.
        kind: String,
        /// Provider output position.
        output_index: u32,
    },
    /// One normalized provider-executed tool call and optional settlement.
    ServerTool {
        /// Provider call identity.
        call_id: heycode_core::CallId,
        /// Stable logical capability.
        logical: String,
        /// Provider-native tool label.
        provider_name: String,
        /// Bounded normalized settlement; raw provider output is absent.
        result: Option<heycode_core::ServerToolResult>,
    },
    /// Provider aggregate usage that cannot honestly be converted into calls.
    ServerToolUsage {
        /// Logical capability.
        logical: String,
        /// Provider-reported request count.
        requests: u32,
        /// Unknown or exact published-cost label.
        cost: String,
    },
    /// One durable public citation card.
    Citation {
        /// Public HTTP(S) URL.
        url: String,
        /// Optional source title.
        title: Option<String>,
        /// Optional bounded excerpt.
        cited_text: Option<String>,
        /// Optional output range start.
        start_index: Option<u32>,
        /// Optional output range end.
        end_index: Option<u32>,
    },
    /// One durable revision-bound model finding report. The complete report
    /// remains retained while the transcript renders a bounded local-only card.
    FindingsReport {
        /// Complete structured report committed to session truth.
        report: Box<heycode_session::FindingReport>,
        /// Show bounded trigger/failure/impact details.
        expanded: bool,
        /// Keyboard focus on this report's disclosure header.
        focused: bool,
    },
    /// One portable or exact-route native compaction checkpoint.
    Compaction {
        /// Native exact-route checkpoint rather than portable summary.
        native: bool,
        /// Agent-owned strategy id for native checkpoints.
        strategy: Option<String>,
        /// Highest replaced durable sequence.
        replaced_upto_seq: u64,
        /// Portable model-visible summary; absent for native state.
        summary: Option<String>,
        /// Exact provider-state item count, never the opaque items themselves.
        provider_items: usize,
        /// User-controlled disclosure; durable summary bytes remain unchanged.
        expanded: bool,
        /// Keyboard/pointer focus on the compaction receipt.
        focused: bool,
    },
    /// Durable link to a provider-native runtime session. The provider-native
    /// session id is intentionally not displayed.
    RuntimeLink {
        /// Runtime registry id.
        runtime: String,
    },
    /// Durable request route changed within this session.
    RouteChange {
        /// New provider.
        provider: String,
        /// New canonical model.
        model: String,
    },
    /// Durable plan-mode state change.
    PlanMode {
        /// New active state.
        active: bool,
    },
    /// Durable goal-domain state or clear tombstone.
    Goal {
        /// Mutation verb.
        action: String,
        /// Post-mutation lifecycle phase, absent for clear.
        phase: Option<String>,
        /// Durable objective, absent for clear.
        objective: Option<String>,
        /// Exact goal revision after the mutation.
        revision: u64,
    },
    /// Durable workflow lifecycle/progress card without checkpoint JSON.
    Workflow {
        /// Lifecycle operation.
        action: String,
        /// Observer-safe bounded summary.
        summary: String,
    },
    /// Durable session-local schedule mutation without reminder content.
    Schedule {
        /// Mutation verb.
        action: String,
        /// Safe timing/identity summary.
        summary: String,
    },
    /// Command/side-channel info line.
    Info(String),
    /// A refusal that is about the input rather than about a command that ran.
    ///
    /// An unrecognised slash line never becomes a command, so it has no echo
    /// to hang a receipt under. Claude Code 2.1.269 answers it with a
    /// standalone warning-coloured `⏺ Unknown command: /x` row rather than an
    /// error row, and shows no echo band at all.
    Notice(String),
    /// Error line.
    Error(String),
}

impl Item {
    /// Ordinary in-flight work belongs in the live activity line, not the transcript.
    pub(crate) fn is_quiet_running_tool(&self) -> bool {
        match self {
            Self::Tool {
                result: None, view, ..
            } => {
                view.spawn_children.is_empty()
                    && !view.expanded
                    && !view.approval.as_deref().is_some_and(|approval| {
                        approval.starts_with("awaiting approval")
                            || approval.starts_with("rejected")
                    })
            }
            Self::ServerTool { result: None, .. } => true,
            _ => false,
        }
    }

    /// The transcript renderer draws nothing at all for this item, so it can
    /// never separate a command echo from the receipt that follows it.
    pub(crate) fn renders_no_rows(&self) -> bool {
        self.is_lifecycle_diagnostic()
            || self.is_merged_tool()
            || self.is_group_hidden()
            || self.is_quiet_running_tool()
            || crate::transcript::quiet_orchestration(self)
    }

    pub(crate) fn is_group_hidden(&self) -> bool {
        matches!(self, Self::Tool { view, .. } if view.group_hidden)
            || matches!(self, Self::Reasoning { view, .. } if view.group_hidden)
    }

    pub(crate) fn group_parent(&self) -> Option<usize> {
        match self {
            Self::Tool { view, .. } => view.group_parent,
            Self::Reasoning { view, .. } => view.group_parent,
            _ => None,
        }
    }
    pub(crate) fn is_merged_tool(&self) -> bool {
        matches!(self, Self::Tool { view, .. } if view.merged)
    }

    pub(crate) fn is_lifecycle_diagnostic(&self) -> bool {
        match self {
            Self::RuntimeLink { .. }
            | Self::RouteChange { .. }
            | Self::PlanMode { .. }
            // Successful schedule mutations already have their tool/command
            // receipts. Internal rule/timestamp diagnostics stay in the journal.
            | Self::Schedule { .. } => true,
            Self::Info(text) => [
                "Entering Plan:",
                "plan mode ON",
                "plan mode OFF",
                "Permissions:",
                "Present the full plan via exit_plan_mode",
                "foreground shell ",
                "Started foreground tool ",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix)),
            _ => false,
        }
    }
}

/// Receipt for dismissing the memory chooser without opening a source.
///
/// Wording pinned to the Claude Code 2.1.269 dark, light and no-colour
/// captures of `/memory` followed by Escape.
pub(crate) const CANCELLED_MEMORY_EDITING: &str = "Cancelled memory editing";
/// Receipt for closing the skills panel with no admission change.
///
/// The source draws this for sorting and search changes too: only admission
/// counts as a change.
pub(crate) const SKILLS_PANEL_NO_CHANGES: &str = "No changes";
/// Receipt for dismissing the effort picker without choosing an effort.
pub(crate) const CANCELLED_EFFORT_SELECTION: &str = "Cancelled";

/// Wording for a slash line that names no registered command.
///
/// Matches Claude Code 2.1.269, which answers `/skill-doctor` with
/// `Unknown command: /skill-doctor` and leaves the discovery hint to the
/// palette that is already open above the composer while the line is typed.
pub(crate) fn unknown_command_notice(name: &str) -> String {
    format!("Unknown command: /{name}")
}

/// Events the loop reacts to (terminal input, bus traffic).
pub enum AppEvent {
    /// Terminal input event.
    Terminal(crossterm::event::Event),
    /// Live agent progress.
    Ui(UiEvent),
}

/// Why the interactive shell returned control to its composition owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiRunOutcome {
    /// User chose to leave or the terminal/event source closed.
    Exit,
    /// A first-run authorization flow committed. The owner must tear down the
    /// disconnected world and compose again from durable credential state.
    RecomposeConnection,
    /// A new connection was persisted; restart with a fresh session and without stale route flags.
    RecomposeConnectionSelection,
    /// Logout committed; rebuild mandatory setup and retain any cleanup warning.
    RecomposeLoggedOut {
        /// Safe credential-storage error shown on the fresh welcome screen.
        cleanup_warning: Option<String>,
    },
    /// A named profile was validated by the shared picker store. The owner
    /// must rebuild from the same CLI/store path with this exact name.
    RecomposeProfile {
        /// Stable named-profile file stem; `None` returns to the built-in
        /// composition.
        name: Option<String>,
    },
    /// A durable new/resume/fork selection is ready for the composition owner.
    RecomposeSession(SessionRecompose),
    /// Quiescent restart after the old world's admission has closed.
    RecomposeCurrent {
        /// Durable conversation to reopen after teardown.
        session_id: heycode_core::SessionId,
        /// Requested plugin refresh or terminal presentation.
        action: crate::recomposition::RecompositionAction,
    },
    /// Trust changed after the safe pre-trust world was composed. The owner
    /// must tear that world down and compose again from authoritative state.
    RecomposeWorkspaceTrust {
        /// Committed effective decision.
        decision: heycode_trust::WorkspaceTrustDecision,
        /// Session-only or durable lifetime.
        persistence: heycode_trust::TrustPersistence,
        /// Committed trust-service revision.
        revision: u64,
    },
}

/// Why the composition owner should reopen one already-durable session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRecompose {
    /// `/new` committed a fresh metadata-bearing stream.
    Created {
        /// New durable session id.
        session_id: heycode_core::SessionId,
        /// Local command to retain after reopening the empty conversation.
        command: Option<String>,
    },
    /// The picker or `/resume` validated an existing stream.
    Resume {
        /// Existing durable session id.
        session_id: heycode_core::SessionId,
    },
    /// A shared-prefix child committed before selection publication.
    Forked {
        /// Source session id.
        parent_session_id: heycode_core::SessionId,
        /// Source title captured before the child is created or renamed.
        parent_title: Option<String>,
        /// Newly committed child id.
        session_id: heycode_core::SessionId,
        /// Resolved title of the new branch, when available.
        title: Option<String>,
        /// Validated branch command retained only in the new terminal's UI.
        command: String,
    },
}

/// Highest-priority startup trust modal bound to its live service.
pub struct WorkspaceTrustView {
    prompt: heycode_trust::WorkspaceTrustPrompt,
    selected: usize,
    error: Option<String>,
}

impl WorkspaceTrustView {
    /// Current authoritative typed dialog projection.
    #[must_use]
    pub fn state(&self) -> &heycode_trust::WorkspaceTrustDialogState {
        self.prompt.state()
    }

    /// Highlighted action index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Fixed/path-free action failure, when the modal remains open.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Which error row the current turn has shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnErrorRow {
    /// Nothing shown yet.
    None,
    /// The generic "turn ended with an error" row at this transcript index.
    Generic(usize),
    /// A specific cause was shown; the generic row is suppressed.
    Specific,
}

/// One status-bar context reading plus how much to trust it.
///
/// The token count is either the native agent's heuristic estimate or the
/// last provider-reported prompt size; the two must not render alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextMeter {
    /// Token count shown in the meter.
    pub tokens: u64,
    /// Percent of the configured window. `None` when no window is configured.
    pub percent: Option<u64>,
    /// Whether the reading reached the compaction warn ratio.
    pub warn: bool,
    /// True for the heuristic estimate — renders with a `~`.
    pub estimated: bool,
}

/// Mutable UI state; rendering is a pure function of this.
pub struct AppState {
    /// Transcript row-groups in arrival order.
    pub items: Vec<Item>,
    /// The single-line/multiline editor.
    pub input: tui_textarea::TextArea<'static>,
    pub(crate) voice: crate::voice::VoiceView,
    /// Spinner frame index (braille cycle).
    pub spinner: usize,
    /// Slow, deterministic frame for the header companion's idle animation.
    pub pet_frame: usize,
    /// Local, finite mascot reactions.
    pub companion: crate::mascot::Companion,
    /// Exact visible companion cells from the latest frame.
    pub mascot_hitbox: ratatui::layout::Rect,
    /// Monotonic start of the current turn, used only for elapsed UI time.
    pub turn_started_at: Option<std::time::Instant>,
    /// Current verb shown while busy.
    pub verb: Option<String>,
    /// Last turn usage for the status bar.
    pub usage: Option<heycode_core::TokenUsage>,
    /// Full-context tokens at last turn end (status meter). Either the native
    /// agent's heuristic estimate or the last provider-reported prompt size —
    /// see `context_tokens_estimated` for which.
    pub context_tokens: Option<u64>,
    /// Whether `context_tokens` is a heuristic estimate (`~4` chars/token)
    /// rather than a provider-reported size. Estimates render with a `~`;
    /// unknown provenance defaults to estimated.
    pub context_tokens_estimated: bool,
    /// How the current turn's failure has been shown so far, so a turn renders
    /// exactly one error row whichever order the cause and the settlement
    /// arrive in.
    turn_error: TurnErrorRow,
    /// Structured model context window for the % readout. Absence hides the
    /// percentage rather than borrowing another backend's limit.
    pub context_window: Option<u64>,
    /// Latest shared native request budget.
    pub context_budget: Option<heycode_llm::ContextBudget>,
    /// Fraction of the window at which the meter starts warning — the
    /// compaction threshold minus a margin, so the user sees it coming.
    pub context_warn_ratio: f32,
    /// Active model id for the status bar.
    pub model: String,
    /// Provider-advertised human label for the exact active model id.
    pub model_display_name: Option<String>,
    /// Provider-advertised resolved/canonical model identity, when distinct
    /// from the accepted control value in `model`.
    pub resolved_model: Option<String>,
    /// Provider-advertised model description, when present.
    pub model_description: Option<String>,
    /// Effective provider-native reasoning effort, when the runtime proves an
    /// explicit selection or a model default.
    pub reasoning_effort: Option<String>,
    /// True when `reasoning_effort` came from an advertised model default.
    pub reasoning_effort_is_default: bool,
    configured_reasoning_effort: Option<String>,
    model_default_reasoning_effort: Option<String>,
    /// Active top-level runtime id for welcome/status.
    pub runtime: String,
    /// Active provider id for the status bar.
    pub provider: String,
    /// Effective approval behavior for the status bar.
    pub permission: String,
    /// Working directory for the status bar.
    pub cwd: std::path::PathBuf,
    /// One cached, effect-owned Git/current-PR lookup for the header.
    workspace_context: crate::workspace_context::WorkspaceContextState,
    /// Scroll offset from bottom in rows (0 = follow).
    pub scroll_from_bottom: usize,
    /// Set when the session should end.
    pub quit_requested: bool,
    /// Installed by the runner so Esc/Ctrl+C cancels the active turn.
    pub interrupt_fn: Option<Box<dyn Fn() + Send + Sync>>,
    /// Text captured by Enter, consumed by the runner.
    pub pending_send: Option<String>,
    /// Ephemeral command text/cursor, never a durable argument echo.
    submitted_command_draft: Option<tui_textarea::TextArea<'static>>,
    active_command_draft: Option<tui_textarea::TextArea<'static>>,
    side_command_activity: Option<&'static str>,
    pub(crate) export_panel: Option<crate::export_panel::ConversationExportPanel>,
    pending_plain_text_export: Option<crate::export_panel::PlainTextExportRequest>,
    pub(crate) export_progress: Option<crate::export_panel::PlainTextExportProgress>,
    export_clipboard: bool,
    /// A shortcut-owned policy acknowledgment is already visible in the footer.
    quiet_permission_mode: Option<String>,
    pending_edit_approval_id: Option<u64>,
    /// Images staged for the next non-command message.
    pub pending_attachments: Vec<heycode_core::AttachmentMetadata>,
    /// Live approval dialog (ask mode), if any.
    pub pending_ask: Option<PendingAskView>,
    /// Dedicated full-document plan decision, independent of tool grants.
    pub pending_plan_review: Option<crate::plan_review::PlanReviewView>,
    queued_asks: std::collections::VecDeque<PendingAskView>,
    /// Highest-priority MCP elicitation for this exact session.
    pub pending_mcp_elicitation: Option<PendingMcpElicitationView>,
    queued_mcp_elicitations: std::collections::VecDeque<(u64, heycode_mcp::McpElicitationRequest)>,
    pending_mcp_elicitation_response: Option<(
        u64,
        Result<heycode_mcp::McpElicitationResponse, heycode_mcp::McpElicitationFailure>,
    )>,
    /// Handle to answer dialogs; wired by the runner when ask mode is active.
    pub approvals: Option<std::sync::Arc<heycode_agent::InteractiveApproval>>,
    runtime_permission_ids: HashMap<u64, String>,
    next_runtime_permission_id: u64,
    pending_runtime_permission_response:
        Option<(String, heycode_app_server::AppPermissionDecision)>,
    /// Active delegated-runtime non-secret question dialog.
    pub pending_runtime_question: Option<PendingRuntimeQuestionView>,
    /// Durable nonblocking question cards; opening never changes the composer draft.
    pub optional_questions: optional_questions::OptionalQuestions,
    pending_runtime_question_response: Option<(String, Option<heycode_agent::QuestionAnswer>)>,
    /// Show full reasoning streams (Ctrl+R toggles; default collapsed).
    pub show_reasoning: bool,
    /// Global retained-detail view; does not change durable session content.
    detailed_transcript: bool,
    pub(crate) reasoning_hit_rows: Vec<(u16, usize)>,
    pub(crate) transcript_area: ratatui::layout::Rect,
    reasoning_focus: Option<usize>,
    pub(crate) reasoning_reveal: Option<(usize, usize)>,
    /// Active/inactive onboarding view, when the plugin is composed.
    pub onboarding: Option<heycode_onboarding::OnboardingSnapshot>,
    /// Semantic wizard result for connector plugins/the loop to consume.
    pub onboarding_outcome: Option<heycode_onboarding::OnboardingOutcome>,
    /// Safe connector/authorization status rendered inside onboarding.
    pub onboarding_notice: Option<String>,
    onboarding_service: Option<Arc<heycode_onboarding::OnboardingService>>,
    /// Live masked secret-input card.
    pub pending_secret: Option<PendingSecretView>,
    /// Searchable command palette modal.
    pub command_palette: Option<CommandPaletteView>,
    /// Strict named-profile picker modal.
    profile_picker: Option<ProfilePickerView>,
    profile_service: Option<Arc<heycode_config::NamedProfileService>>,
    current_profile: Option<String>,
    /// Effective-state welcome card shown until transcript content exists.
    pub welcome: Option<WelcomeStatusView>,
    workspace_trust: Option<WorkspaceTrustView>,
    run_outcome: Option<TuiRunOutcome>,
    /// Live model picker modal.
    pub model_picker: Option<ModelPickerView>,
    catalog_overrides: Option<Arc<heycode_catalog_file::CatalogOverrides>>,
    model_refresh_request: Option<heycode_llm::CatalogRefreshMode>,
    model_refresh_cancel_requested: bool,
    pending_model_selection: Option<ModelPickerSelection>,
    /// Exact reasoning-effort picker for the active backend.
    pub effort_picker: Option<EffortPickerView>,
    pending_effort_selection: Option<(
        BackendControlOwner,
        u64,
        String,
        heycode_routing::SelectionScope,
    )>,
    /// Combined inference-provider/agent-runtime picker modal.
    pub route_picker: Option<RoutePickerView>,
    route_picker_request: Option<(String, String)>,
    pending_route_selection: Option<RoutePickerSelection>,
    /// Installed-plugin lifecycle panel.
    plugin_panel: Option<PluginPanelView>,
    plugin_lifecycle: Option<Arc<heycode_extensions::lifecycle::PluginLifecycle>>,
    plugin_packages: Option<Arc<PluginPackageIndex>>,
    /// Read-only skill/agent/hook capability catalog.
    capability_catalog: Option<CapabilityCatalogView>,
    skills_panel: Option<crate::skills_panel::SkillsPanel>,
    agent_readiness: Option<crate::agent_readiness::AgentReadinessProbes>,
    skills: Option<Arc<heycode_skills::SkillSet>>,
    subagents: Option<Arc<heycode_agent::SubagentRegistry>>,
    hooks: Option<Arc<heycode_hooks::HookService>>,
    /// Optional U22 side panel plus its effect-owned job source.
    side_panel: Option<SidePanelKind>,
    jobs: Option<Arc<heycode_agent::JobRegistry>>,
    pub(crate) task_console: crate::task_console::TaskConsole,
    pub(crate) workflow_console: crate::workflow_console::WorkflowConsole,
    inbox_transcript: inbox_transcript::InboxTranscript,
    parent_task_draft: Option<task_navigation::ParentTaskDraft>,
    /// MCP server management panel.
    mcp_panel: Option<McpPanelView>,
    mcp_management: Option<Arc<heycode_mcp::management::McpManagement>>,
    mcp_runtime_control: Option<Arc<heycode_mcp::McpRuntimeControl>>,
    mcp_registry: Option<Arc<heycode_mcp::McpRegistry>>,
    /// Schema-derived/custom settings browser.
    settings_panel: Option<SettingsShellView>,
    settings_service: Option<Arc<heycode_settings::SettingsService>>,
    settings_ui: Option<Arc<heycode_ui::settings_ui::SettingsUiRegistry>>,
    theme_picker: Option<ThemePickerView>,
    keymap_picker: Option<KeymapPickerView>,
    pub(crate) help_panel: Option<crate::help::HelpView>,
    pub(crate) skill_doctor_panel: Option<crate::skill_doctor_panel::SkillDoctorPanel>,
    pub(crate) memory_panel: Option<crate::memory_panel::MemoryPanelView>,
    pub(crate) copy_panel: Option<crate::copy_panel::CopyPanel>,
    pub(crate) add_directory_dialog: Option<crate::add_directory::AddDirectoryDialog>,
    memory_sources: Option<Arc<crate::memory_commands::MemorySourceManager>>,
    scroll_speed_picker: Option<ScrollSpeedPickerView>,
    /// Bounded session picker/lifecycle surface.
    session_browser: Option<SessionBrowserView>,
    session_query: Option<Arc<heycode_session::SessionQueryService>>,
    current_session: Option<Arc<std::sync::Mutex<heycode_session::Session>>>,
    new_session_metadata: Option<heycode_session::SessionCreationMetadata>,
    /// Effective sandbox capability picker modal.
    pub permission_picker: Option<PermissionPickerView>,
    sandbox_panel: Option<crate::sandbox_panel::SandboxPanel>,
    autocompact_panel: Option<crate::autocompact_panel::AutoCompactPanel>,
    pending_sandbox_selection: Option<heycode_exec::SandboxMode>,
    /// Interrupt confirmation for an active-turn command.
    pub pending_command_confirmation: Option<PendingCommandConfirmation>,
    queued_commands: std::collections::VecDeque<QueuedCommand>,
    active_turn: bool,
    /// Transcript boundary captured by the actual scheduled parent turn.
    pub(crate) activity_item_start: usize,
    pub(crate) cancellation_requested: bool,
    pending_inbox_submission: std::collections::VecDeque<(heycode_session::InboxDelivery, String)>,
    inbox_pending: heycode_agent::InboxPending,
    follow_up_wake_pending: bool,
    command_registry: Option<Arc<CommandRegistry>>,
    secret_prompt: Option<Arc<heycode_authorization_api_key::InteractiveSecretPrompt>>,
    ctrl_c_seen: bool,
    /// Whether a standalone `?` has replaced the idle footer with the
    /// shortcut list. Only meaningful while the composer is empty.
    shortcut_list_open: bool,
    /// Prompts this session has sent, oldest first, for Up/Down recall.
    prompt_history: Vec<String>,
    /// While walking history: the index shown and the draft to restore.
    history_cursor: Option<(usize, Vec<String>)>,
    /// Durable prompt history, when this surface has a home to keep one in.
    prompt_history_store: Option<Arc<crate::prompt_history::PromptHistoryStore>>,
    /// Role colours resolved for the detected colour tier.
    styles: crate::terminal::Styles,
    /// Decoration the detected render mode permits.
    chrome: crate::terminal::Chrome,
    /// Settings-backed persistent header/footer choices.
    shell_preferences: heycode_ui::preferences::ShellChromePreferences,
    /// Mouse-wheel speed in quarter steps; 4 is the default 1x multiplier.
    scroll_speed_quarters: u8,
    /// Signed quarter-row residue retained across wheel events.
    scroll_wheel_remainder: i16,
    /// Claude-style compact projection of only the current conversational turn.
    focus_view: bool,
    /// Optional per-session prompt-bar accent; never changes the global theme.
    prompt_color: Option<crate::human_commands::PromptColor>,
    /// Latest durable title event for the current session.
    current_session_title: Option<String>,
    /// Live key bindings; defaults until a persisted keymap is applied.
    keymap: heycode_ui::keymap::Keymap,
    terminal_capabilities: Option<heycode_ui::terminal::TerminalCapabilities>,
    vim_enabled: bool,
    vim_insert: bool,
    pending_clipboard: Option<String>,
    pending_copy_recovery: Option<(crate::copy_panel::CopySelection, bool)>,
    pending_delivery_save: Option<Vec<crate::file_delivery::DeliveredFile>>,
    pub(crate) screen_selection: crate::screen_selection::ScreenSelection,
    pub(crate) copy_notice: Option<(String, std::time::Instant)>,
    human_commands: crate::human_commands::HumanCommandBridge,
    advisor_bridge: crate::advisor_panel::AdvisorPanelBridge,
    advisor_control: Option<(
        Arc<heycode_agent::AdvisorService>,
        Arc<heycode_agent::Agent>,
    )>,
    pub(crate) advisor_panel: Option<crate::advisor_panel::AdvisorPanelView>,
    pub(crate) rewind_picker: Option<crate::rewind_picker::RewindPicker>,
    pub(crate) transcript_cache: crate::transcript::TranscriptRenderCache,
    transcript_style_generation: u64,
    tool_items: HashMap<String, usize>,
    server_tool_items: HashMap<String, usize>,
    live_assistant_item: HashMap<(u64, u32), usize>,
    live_reasoning_item: HashMap<(u64, u32), usize>,
    tool_group_signature: Option<u64>,
    last_request_route: Option<(String, String)>,
    /// Panel-open inbox shared with the CMD04 capability commands.
    panel_commands: PanelCommandBridge,
}

/// Ranked rows and highlight for the command palette modal.
///
/// The composer owns the text: the palette is a view over the first token
/// after a leading `/`, refreshed as the user types. `query` caches that token
/// for renderers.
pub struct CommandPaletteView {
    query: String,
    matches: Vec<CommandPaletteMatch>,
    /// Highlighted row, or `NO_HIGHLIGHT` when no row is preselectable and the
    /// user has not arrowed to one.
    selected: usize,
    /// The user moved the highlight explicitly, so a description-only row may
    /// be run on Enter.
    navigated: bool,
    /// A non-slash draft displaced by Ctrl+P, restored when the palette closes
    /// without running anything.
    restore_draft: Option<tui_textarea::TextArea<'static>>,
}

/// Highlight index that matches no row.
const NO_HIGHLIGHT: usize = usize::MAX;

/// Sorted strict named-profile rows and current selection.
pub struct ProfilePickerView {
    rows: Vec<ProfilePickerRow>,
    selected: usize,
}

/// One choice in the profile picker: a named profile from the shared store,
/// or the built-in composition (no `--profile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePickerRow {
    /// Named-profile file stem; `None` is the built-in composition.
    pub name: Option<String>,
    /// Whether this is what the running world was composed from.
    pub current: bool,
}

impl ProfilePickerRow {
    /// Text shown for the row.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("built-in (no profile)")
    }
}

/// Keyboard-complete theme preview backed by one exact Settings revision.
pub struct ThemePickerView {
    themes: Vec<heycode_ui::theme::Theme>,
    selected: usize,
    original: usize,
    revision: u64,
}

impl ThemePickerView {
    /// Live theme rows.
    #[must_use]
    pub fn themes(&self) -> &[heycode_ui::theme::Theme] {
        &self.themes
    }

    /// Highlighted/previewed row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Row committed in Settings when the picker opened.
    ///
    /// Previewing moves [`Self::selected`] and repaints the shell, so the
    /// panel needs this separate index to keep the `✔` on the theme that is
    /// actually persisted until Enter commits a new one.
    #[must_use]
    pub const fn current(&self) -> usize {
        self.original
    }
}

/// Shortcut browser. Enter transfers the selected action to an editable
/// `/keymap <action> ` command, keeping mutation on the Settings CAS command.
pub struct KeymapPickerView {
    rows: Vec<(
        heycode_ui::keymap::KeymapAction,
        heycode_ui::keymap::KeyChord,
    )>,
    selected: usize,
    revision: u64,
}

/// Keyboard-complete mouse-wheel speed picker with an in-place ruler preview.
pub struct ScrollSpeedPickerView {
    quarters: u8,
    revision: u64,
    preview_offset: i32,
    preview_remainder: i16,
}

impl ScrollSpeedPickerView {
    /// Current multiplier encoded as quarter steps.
    #[must_use]
    pub const fn quarters(&self) -> u8 {
        self.quarters
    }

    /// Current multiplier as a display value.
    #[must_use]
    pub const fn speed(&self) -> f32 {
        self.quarters as f32 / 4.0
    }

    /// Signed ruler position moved by preview wheel events.
    #[must_use]
    pub const fn preview_offset(&self) -> i32 {
        self.preview_offset
    }

    /// Settings revision this picker was opened from.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl KeymapPickerView {
    /// Action/chord rows in closed action order.
    #[must_use]
    pub fn rows(
        &self,
    ) -> &[(
        heycode_ui::keymap::KeymapAction,
        heycode_ui::keymap::KeyChord,
    )] {
        &self.rows
    }

    /// Highlighted row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Settings revision this browser was opened from.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl ProfilePickerView {
    /// The built-in row first, then the strict rows returned by the shared
    /// store.
    #[must_use]
    pub fn rows(&self) -> &[ProfilePickerRow] {
        &self.rows
    }

    /// Highlighted row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
}

impl CommandPaletteView {
    /// Current fuzzy query without the leading slash.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Ranked registry rows.
    #[must_use]
    pub fn matches(&self) -> &[CommandPaletteMatch] {
        &self.matches
    }

    /// Highlighted row index; unreachable when nothing is highlighted, so
    /// renderers comparing `index == selected()` highlight no row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// The highlighted row, if any.
    #[must_use]
    pub fn highlighted(&self) -> Option<usize> {
        (self.selected < self.matches.len()).then_some(self.selected)
    }
}

/// The palette query: the first token after a leading `/` on a single-line
/// composer, or `None` when the composer is not a slash line.
fn palette_query<'a>(input: &'a tui_textarea::TextArea<'_>) -> Option<&'a str> {
    let lines = input.lines();
    let [line] = lines else {
        return None;
    };
    let rest = line.strip_prefix('/')?;
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// Index of the first row the palette may highlight without navigation.
fn first_preselectable(matches: &[CommandPaletteMatch]) -> usize {
    matches
        .iter()
        .position(|row| row.preselectable)
        .unwrap_or(NO_HIGHLIGHT)
}

/// The first eight characters of a session id, for human-facing notices.
fn short_session_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Doctor projection shown on the welcome card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WelcomeHealth {
    /// Doctor task has not settled.
    Checking,
    /// No check failed/skipped; warnings remain visible.
    Healthy {
        /// Passing check count.
        passed: usize,
        /// Warning check count.
        warnings: usize,
    },
    /// One or more checks failed or were skipped.
    Unhealthy {
        /// Failing check count.
        failed: usize,
        /// Skipped check count.
        skipped: usize,
    },
    /// No doctor service was composed.
    Unavailable,
}

/// Effective runtime/route/policy/workspace snapshot for startup.
pub struct WelcomeStatusView {
    runtime: String,
    provider: String,
    model: String,
    permission: String,
    workspace: std::path::PathBuf,
    health: WelcomeHealth,
}

/// Model picker catalog-loading state.
pub enum ModelPickerLoadState {
    /// Initial/forced refresh in progress.
    Loading,
    /// One catalog generation is available.
    Ready {
        /// Safe immutable provider generation.
        snapshot: Arc<heycode_llm::CatalogSnapshot>,
        /// Live/cache/stale provenance.
        freshness: heycode_llm::CatalogFreshness,
        /// Visible stale warning, if any.
        warning: Option<String>,
    },
    /// No generation is available.
    Error {
        /// Safe catalog error.
        message: String,
    },
}

/// Search/filter/selection state for one provider model catalog.
pub struct ModelPickerView {
    owner: BackendControlOwner,
    routing_revision: u64,
    current_model: String,
    query: String,
    search_active: bool,
    filter: ModelPickerFilter,
    matches: Vec<ModelPickerMatch>,
    selected: usize,
    unmatched_overrides: Vec<String>,
    state: ModelPickerLoadState,
    effort_model: Option<String>,
    effort_picker: Option<EffortPickerView>,
    effort_error: Option<String>,
}

/// One reviewed picker choice and the owner token captured when it opened.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPickerSelection {
    /// Native provider or delegated runtime that owns these controls.
    pub owner: BackendControlOwner,
    /// Routing revision used to reject stale commits.
    pub revision: u64,
    /// Catalog evidence for the selected model.
    pub catalog: Arc<heycode_llm::CatalogSnapshot>,
    /// Exact provider-owned model ID.
    pub model: String,
    /// Save a default or apply only to this composition.
    pub scope: heycode_routing::SelectionScope,
    /// Effort shown for this model, when metadata is available.
    pub effort: Option<String>,
}

/// Selection state for exact backend-owned reasoning effort values.
pub struct EffortPickerView {
    owner: BackendControlOwner,
    routing_revision: u64,
    current_effort: Option<String>,
    choices: Vec<String>,
    default_effort: Option<String>,
    selected: usize,
}

struct ModelConfigurationFields<'a> {
    model: &'a str,
    display_name: &'a str,
    resolved_model: Option<&'a str>,
    description: Option<&'a str>,
    context_window: Option<u64>,
    default_reasoning_effort: Option<&'a str>,
    reasoning_efforts: &'a [String],
}

/// Search/filter/selection state for the combined route picker.
pub struct RoutePickerView {
    current_provider: String,
    current_runtime: String,
    query: String,
    filter: RoutePickerFilter,
    rows: Vec<RoutePickerRow>,
    matches: Vec<RoutePickerMatch>,
    selected: usize,
    loading: bool,
    error: Option<String>,
}

/// Effective policy/backend state for the permission picker.
pub struct PermissionPickerView {
    rows: Vec<PermissionPickerRow>,
    selected: usize,
}
impl PermissionPickerView {
    /// The four permission choices.
    #[must_use]
    pub fn rows(&self) -> &[PermissionPickerRow] {
        &self.rows
    }
    /// Highlighted choice.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
}

impl RoutePickerView {
    /// Current inference-provider id.
    #[must_use]
    pub fn current_provider(&self) -> &str {
        &self.current_provider
    }

    /// Current top-level runtime id.
    #[must_use]
    pub fn current_runtime(&self) -> &str {
        &self.current_runtime
    }

    /// Fuzzy query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Active class filter.
    #[must_use]
    pub const fn filter(&self) -> RoutePickerFilter {
        self.filter
    }

    /// Ranked visible rows.
    #[must_use]
    pub fn matches(&self) -> &[RoutePickerMatch] {
        &self.matches
    }

    /// Highlighted row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Whether registry projection is pending.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        self.loading
    }

    /// Safe registry projection error.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

impl ModelPickerView {
    /// Effort values belonging to the highlighted model.
    #[must_use]
    pub fn effort_picker(&self) -> Option<&EffortPickerView> {
        self.effort_picker.as_ref()
    }

    /// Why the highlighted model has no adjustable effort.
    #[must_use]
    pub fn effort_error(&self) -> Option<&str> {
        self.effort_error.as_deref()
    }
    /// Provider/runtime whose catalog is shown.
    #[must_use]
    pub fn provider(&self) -> &str {
        self.owner.id()
    }

    /// Backend that owns discovery and application for this picker.
    #[must_use]
    pub const fn owner(&self) -> &BackendControlOwner {
        &self.owner
    }

    /// Current model before selection.
    #[must_use]
    pub fn current_model(&self) -> &str {
        &self.current_model
    }

    /// Fuzzy query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Active evidence filter.
    #[must_use]
    pub const fn filter(&self) -> ModelPickerFilter {
        self.filter
    }

    /// Ranked model rows.
    #[must_use]
    pub fn matches(&self) -> &[ModelPickerMatch] {
        &self.matches
    }

    /// Highlighted row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Loading/ready/error state.
    #[must_use]
    pub const fn state(&self) -> &ModelPickerLoadState {
        &self.state
    }

    /// Override rows that matched no canonical provider model.
    #[must_use]
    pub fn unmatched_overrides(&self) -> &[String] {
        &self.unmatched_overrides
    }
}

impl EffortPickerView {
    fn new(
        owner: BackendControlOwner,
        routing_revision: u64,
        current_effort: Option<String>,
        mut choices: Vec<String>,
        default_effort: Option<String>,
    ) -> Self {
        let rank = |value: &str| match value {
            "none" | "off" => Some(0),
            "minimal" => Some(1),
            "low" => Some(2),
            "medium" => Some(3),
            "high" => Some(4),
            "xhigh" => Some(5),
            "max" => Some(6),
            _ => None,
        };
        if choices.iter().all(|value| rank(value).is_some()) {
            choices.sort_by_key(|value| rank(value));
        }
        let selected = current_effort
            .as_deref()
            .and_then(|current| choices.iter().position(|choice| choice == current))
            .or_else(|| {
                default_effort
                    .as_deref()
                    .and_then(|default| choices.iter().position(|choice| choice == default))
            })
            .unwrap_or(0);
        Self {
            owner,
            routing_revision,
            current_effort,
            choices,
            default_effort,
            selected,
        }
    }
    /// Backend that owns this exact effort list.
    #[must_use]
    pub const fn owner(&self) -> &BackendControlOwner {
        &self.owner
    }

    /// Explicit current effort, absent when the backend default is active.
    #[must_use]
    pub fn current_effort(&self) -> Option<&str> {
        self.current_effort.as_deref()
    }

    /// Backend default effort when explicitly advertised.
    #[must_use]
    pub fn default_effort(&self) -> Option<&str> {
        self.default_effort.as_deref()
    }

    /// Exact selectable effort ids in backend display order.
    #[must_use]
    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    /// Highlighted effort index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
}

impl WelcomeStatusView {
    /// Construct a checking welcome snapshot from effective service state.
    #[must_use]
    pub fn new(
        runtime: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        permission: impl Into<String>,
        workspace: std::path::PathBuf,
    ) -> Self {
        Self {
            runtime: runtime.into(),
            provider: provider.into(),
            model: model.into(),
            permission: permission.into(),
            workspace,
            health: WelcomeHealth::Checking,
        }
    }

    /// Effective runtime id.
    #[must_use]
    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    /// Effective provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Effective model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Effective approval behavior.
    #[must_use]
    pub fn permission(&self) -> &str {
        &self.permission
    }

    /// Effective workspace path.
    #[must_use]
    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }

    /// Current health projection.
    #[must_use]
    pub const fn health(&self) -> &WelcomeHealth {
        &self.health
    }
}

/// Cancel-default confirmation before an interrupting command cancels a turn.
pub struct PendingCommandConfirmation {
    /// Exact command text submitted by the user.
    pub text: String,
    /// Human command synopsis.
    pub synopsis: String,
    /// 0 = Cancel, 1 = Interrupt & run.
    pub selection: usize,
}

struct QueuedCommand {
    text: String,
    synopsis: String,
}

/// The dialog card state: what is asked and which option is highlighted.
/// Whether a panel listing connects to servers or only reads definitions.
///
/// Kept explicit at every call site: "did this key press start eight
/// processes?" must be answerable by reading the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpListingDepth {
    /// Store rows only; health stays unknown.
    Stored,
    /// Store rows plus one concurrent live probe each.
    Probed,
}

/// The approval card on screen: what is being asked, which choice is
/// highlighted, and the denial reason being typed when the editor is open.
pub struct PendingAskView {
    /// Child session that owns this request; absent for the root.
    pub owner_session: Option<String>,
    /// Friendly child name from the authoritative task inventory.
    pub owner_label: Option<String>,
    owner_key: Option<crate::task_console::TaskKey>,
    /// Policy-side id to answer with.
    pub id: u64,
    /// Tool name.
    pub name: String,
    /// Argument preview.
    pub args_preview: String,
    /// Highlighted option index in the active mode's choices (arrow keys move; `y` / `a` / `n` / `r`
    /// jump).
    pub selection: usize,
    /// One-line denial reason being typed. `Some` means the card's reason
    /// editor is open and owns the keyboard; `None` means the choice list is
    /// active.
    reason: Option<String>,
    tool_index: Option<usize>,
    /// Structured diff admitted only from an exact, complete retained Read.
    pub(crate) edit_preview: Option<crate::approval_preview::EditApprovalPreview>,
}

impl PendingAskView {
    /// A fresh card for one request, with the first choice highlighted and
    /// the reason editor closed.
    #[must_use]
    pub fn new(id: u64, name: impl Into<String>, args_preview: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            args_preview: args_preview.into(),
            selection: 0,
            reason: None,
            tool_index: None,
            edit_preview: None,
            owner_session: None,
            owner_label: None,
            owner_key: None,
        }
    }

    /// The reason text while the editor is open.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// The choices an approval card offers, in display order.
pub const ASK_CHOICES: [&str; 3] = ["Accept", "Allow identical calls this session", "Reject"];
/// Explicitly enter Accepted edits from a native Default-mode approval.
pub const ASK_EDIT_CHOICES: [&str; 3] = ["Accept", "Accept + allow edits this session", "Reject"];
/// Choices for Default mode, which never remembers permissions.
pub const ASK_DEFAULT_CHOICES: [&str; 2] = ["Accept", "Reject"];

/// One MCP server elicitation routed to this exact session.
pub struct PendingMcpElicitationView {
    /// TUI-local correlation id.
    pub id: u64,
    /// Validated server id.
    pub server: String,
    /// Bounded server explanation.
    pub message: String,
    /// Mode-specific interaction state.
    pub mode: PendingMcpElicitationMode,
}

/// Mode-specific MCP elicitation state.
pub enum PendingMcpElicitationMode {
    /// Flat primitive form entered as one JSON object.
    Form {
        /// Strict validated form schema.
        schema: serde_json::Value,
        /// Human-edited JSON object.
        input: String,
        /// Parse error retained without closing the request.
        error: Option<String>,
    },
    /// Credential-free HTTPS flow acknowledged out of band.
    Url {
        /// Validated public URL.
        url: String,
    },
}

/// One delegated-runtime question with choices or bounded free text.
pub struct PendingRuntimeQuestionView {
    /// Explicit requested answer shape.
    pub mode: heycode_core::QuestionMode,
    /// One-based batch position and total.
    pub progress: (usize, usize),
    /// Explicitly toggled choices, distinct from cursor focus.
    pub selected_choices: std::collections::BTreeSet<usize>,
    /// Provider-native request correlation.
    pub request_id: String,
    /// Optional short category label.
    pub header: Option<String>,
    /// Safe prompt.
    pub prompt: String,
    /// Ordered choices; empty enables free text.
    pub choices: Vec<String>,
    /// Explanations aligned one-for-one with `choices`.
    pub choice_descriptions: Vec<Option<String>>,
    /// Selected choice index.
    pub selection: usize,
    /// Non-secret free-text answer.
    pub input: String,
}

/// Masked secret-input view. The raw input is private and zeroized on drop.
pub struct PendingSecretView {
    /// Broker request id.
    pub id: u64,
    /// Human prompt.
    pub prompt: String,
    /// Safe validation feedback for a new empty field.
    pub error: Option<String>,
    /// Non-secret reference name.
    pub reference: String,
    /// Render masking requirement.
    pub masked: bool,
    secret: String,
}

impl PendingSecretView {
    pub(crate) fn masked_preview(&self, width: usize) -> String {
        let count = self.secret.chars().count();
        if count <= 6 {
            return "•".repeat(count.min(width));
        }
        let prefix: String = self.secret.chars().take(5).collect();
        let last = self.secret.chars().last().unwrap_or_default();
        format!(
            "{prefix}{}{last}",
            "•".repeat((count - 6).min(width.saturating_sub(6)))
        )
    }
}

impl Drop for PendingSecretView {
    fn drop(&mut self) {
        use secrecy::zeroize::Zeroize as _;
        self.secret.zeroize();
    }
}

const SPINNER_FRAMES: [&str; 8] = ["✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳"];
const SPINNER_VERBS: [&str; 6] = [
    "Thinking…",
    "Reading…",
    "Searching…",
    "Forging…",
    "Polishing…",
    "Wrestling code…",
];

/// Render a wheel multiplier without trailing zeroes, e.g. `1`, `1.5`, `0.25`.
#[must_use]
pub fn format_scroll_speed(speed: f32) -> String {
    if speed.fract() == 0.0 {
        format!("{speed:.0}")
    } else {
        format!("{speed:.2}").trim_end_matches('0').to_owned()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            input: tui_textarea::TextArea::default(),
            voice: crate::voice::VoiceView::default(),
            spinner: 0,
            pet_frame: 0,
            companion: crate::mascot::Companion::default(),
            mascot_hitbox: ratatui::layout::Rect::default(),
            turn_started_at: None,
            verb: None,
            usage: None,
            context_tokens: None,
            context_tokens_estimated: true,
            turn_error: TurnErrorRow::None,
            context_window: None,
            context_budget: None,
            context_warn_ratio: 0.7,
            model: String::new(),
            model_display_name: None,
            resolved_model: None,
            model_description: None,
            reasoning_effort: None,
            reasoning_effort_is_default: false,
            configured_reasoning_effort: None,
            model_default_reasoning_effort: None,
            runtime: String::new(),
            provider: String::new(),
            permission: String::new(),
            cwd: std::path::PathBuf::from("."),
            workspace_context: crate::workspace_context::WorkspaceContextState::Loading,
            scroll_from_bottom: 0,
            quit_requested: false,
            interrupt_fn: None,
            pending_send: None,
            submitted_command_draft: None,
            active_command_draft: None,
            side_command_activity: None,
            export_panel: None,
            pending_plain_text_export: None,
            export_progress: None,
            export_clipboard: false,
            quiet_permission_mode: None,
            pending_edit_approval_id: None,
            pending_attachments: Vec::new(),
            pending_ask: None,
            pending_plan_review: None,
            queued_asks: std::collections::VecDeque::new(),
            pending_mcp_elicitation: None,
            queued_mcp_elicitations: std::collections::VecDeque::new(),
            pending_mcp_elicitation_response: None,
            approvals: None,
            runtime_permission_ids: HashMap::new(),
            next_runtime_permission_id: u64::MAX,
            pending_runtime_permission_response: None,
            pending_runtime_question: None,
            optional_questions: optional_questions::OptionalQuestions::default(),
            pending_runtime_question_response: None,
            show_reasoning: false,
            detailed_transcript: false,
            reasoning_hit_rows: Vec::new(),
            transcript_area: ratatui::layout::Rect::default(),
            reasoning_focus: None,
            reasoning_reveal: None,
            onboarding: None,
            onboarding_outcome: None,
            onboarding_notice: None,
            onboarding_service: None,
            pending_secret: None,
            command_palette: None,
            profile_picker: None,
            profile_service: None,
            current_profile: None,
            welcome: None,
            workspace_trust: None,
            run_outcome: None,
            model_picker: None,
            catalog_overrides: None,
            model_refresh_request: None,
            model_refresh_cancel_requested: false,
            pending_model_selection: None,
            effort_picker: None,
            pending_effort_selection: None,
            route_picker: None,
            route_picker_request: None,
            pending_route_selection: None,
            mcp_panel: None,
            mcp_management: None,
            mcp_runtime_control: None,
            mcp_registry: None,
            settings_panel: None,
            settings_service: None,
            settings_ui: None,
            theme_picker: None,
            keymap_picker: None,
            help_panel: None,
            memory_panel: None,
            skill_doctor_panel: None,
            copy_panel: None,
            add_directory_dialog: None,
            memory_sources: None,
            scroll_speed_picker: None,
            session_browser: None,
            session_query: None,
            current_session: None,
            new_session_metadata: None,
            plugin_panel: None,
            plugin_lifecycle: None,
            plugin_packages: None,
            capability_catalog: None,
            skills_panel: None,
            agent_readiness: None,
            skills: None,
            subagents: None,
            hooks: None,
            side_panel: None,
            jobs: None,
            task_console: crate::task_console::TaskConsole::default(),
            inbox_transcript: inbox_transcript::InboxTranscript::default(),
            workflow_console: crate::workflow_console::WorkflowConsole::default(),
            parent_task_draft: None,
            permission_picker: None,
            sandbox_panel: None,
            autocompact_panel: None,
            pending_sandbox_selection: None,
            pending_command_confirmation: None,
            queued_commands: std::collections::VecDeque::new(),
            active_turn: false,
            activity_item_start: 0,
            cancellation_requested: false,
            pending_inbox_submission: std::collections::VecDeque::new(),
            inbox_pending: heycode_agent::InboxPending::default(),
            follow_up_wake_pending: false,
            command_registry: None,
            secret_prompt: None,
            ctrl_c_seen: false,
            shortcut_list_open: false,
            prompt_history: Vec::new(),
            history_cursor: None,
            prompt_history_store: None,
            // A state nobody has told about a terminal renders exactly as this
            // build always has: the built-in theme at 24-bit, full chrome and
            // the shipped bindings. Detection happens in `run_interactive`,
            // which is the only place that has a terminal to look at.
            styles: crate::terminal::Styles::default(),
            chrome: crate::terminal::Chrome::default(),
            shell_preferences: heycode_ui::preferences::ShellChromePreferences::default(),
            scroll_speed_quarters: 4,
            scroll_wheel_remainder: 0,
            focus_view: false,
            prompt_color: None,
            current_session_title: None,
            keymap: heycode_ui::keymap::Keymap::defaults(),
            terminal_capabilities: None,
            vim_enabled: false,
            vim_insert: true,
            pending_clipboard: None,
            pending_copy_recovery: None,
            pending_delivery_save: None,
            screen_selection: crate::screen_selection::ScreenSelection::default(),
            copy_notice: None,
            human_commands: crate::human_commands::HumanCommandBridge::new(),
            advisor_bridge: crate::advisor_panel::AdvisorPanelBridge::new(),
            advisor_control: None,
            advisor_panel: None,
            rewind_picker: None,
            transcript_cache: crate::transcript::TranscriptRenderCache::default(),
            transcript_style_generation: 0,
            tool_items: HashMap::new(),
            server_tool_items: HashMap::new(),
            live_assistant_item: HashMap::new(),
            live_reasoning_item: HashMap::new(),
            tool_group_signature: None,
            last_request_route: None,
            panel_commands: PanelCommandBridge::new(),
        }
    }
}

impl AppState {
    /// Fresh state bound to a model id and working directory.
    #[must_use]
    pub fn new(model: impl Into<String>, cwd: std::path::PathBuf) -> Self {
        Self {
            model: model.into(),
            cwd,
            ..Self::default()
        }
    }

    /// Attach the live command registry used whenever the palette opens.
    pub fn set_commands(&mut self, commands: Arc<CommandRegistry>) {
        self.command_registry = Some(commands);
    }

    /// Attach the effect-owned named-profile service used by `/profile`.
    pub fn set_profiles(
        &mut self,
        profiles: Arc<heycode_config::NamedProfileService>,
        current: Option<String>,
    ) {
        self.profile_service = Some(profiles);
        self.current_profile = current;
    }

    /// Current named-profile picker, when open.
    #[must_use]
    pub const fn profile_picker(&self) -> Option<&ProfilePickerView> {
        self.profile_picker.as_ref()
    }

    fn open_profile_picker(&mut self) {
        self.close_capability_catalog();
        self.close_settings_panel();
        self.session_browser = None;
        if self.workspace_trust.is_some()
            || self.pending_secret.is_some()
            || self.pending_ask.is_some()
            || self.pending_command_confirmation.is_some()
            || self.pending_runtime_question.is_some()
            || self
                .onboarding
                .as_ref()
                .is_some_and(|onboarding| onboarding.active)
        {
            return;
        }
        let Some(profiles) = self.profile_service.as_ref() else {
            self.items
                .push(Item::Error("named profiles are unavailable".to_owned()));
            return;
        };
        match profiles.list() {
            Ok(named) => {
                // The built-in composition is always a valid target, so a user
                // on a profile can get back without restarting by hand.
                let rows: Vec<ProfilePickerRow> = std::iter::once(ProfilePickerRow {
                    name: None,
                    current: self.current_profile.is_none(),
                })
                .chain(named.into_iter().map(|row| ProfilePickerRow {
                    current: self.current_profile.as_deref() == Some(row.name.as_str()),
                    name: Some(row.name),
                }))
                .collect();
                let selected = rows.iter().position(|row| row.current).unwrap_or(0);
                self.close_permission_picker();
                self.close_model_picker();
                self.close_effort_picker();
                self.close_route_picker();
                self.close_mcp_panel();
                self.close_plugin_panel();
                self.command_palette = None;
                self.profile_picker = Some(ProfilePickerView { rows, selected });
            }
            Err(_) => self
                .items
                .push(Item::Error("named profiles are unavailable".to_owned())),
        }
    }

    fn select_profile(&mut self, name: Option<String>) {
        if let Some(name) = name.as_deref() {
            let valid = self
                .profile_service
                .as_ref()
                .is_some_and(|profiles| profiles.load(name).is_ok());
            if !valid {
                self.items.push(Item::Error(format!(
                    "named profile `{name}` is unavailable"
                )));
                return;
            }
        }
        if self.current_profile == name {
            self.profile_picker = None;
            return;
        }
        self.profile_picker = None;
        self.run_outcome = Some(TuiRunOutcome::RecomposeProfile { name });
    }

    /// Share the shell's panel-open inbox with the CMD04 capability commands.
    ///
    /// The bridge is the command plane's only way into this state: a command
    /// records a panel, the loop drains it on the wake that command's own
    /// completion produces. Attachment already recorded on `self` is carried
    /// over so a bridge installed after the services still reports the truth.
    pub fn set_panel_commands(&mut self, panels: PanelCommandBridge) {
        if let Some(management) = self.mcp_management.as_ref() {
            panels.attach_mcp(Arc::clone(management));
        }
        if let Some(control) = self.mcp_runtime_control.as_ref() {
            panels.attach_mcp_runtime_control(Arc::clone(control));
        }
        if self.plugin_lifecycle.is_some() {
            panels.attach(CapabilityPanel::Plugins);
        }
        if self.skills.is_some() {
            panels.attach(CapabilityPanel::Skills);
        }
        if self.subagents.is_some() {
            panels.attach(CapabilityPanel::Agents);
        }
        if self.hooks.is_some() {
            panels.attach(CapabilityPanel::Hooks);
        }
        if self.settings_service.is_some() && self.settings_ui.is_some() {
            panels.attach(CapabilityPanel::Settings);
        }
        self.panel_commands = panels;
    }

    /// Take the panel the human asked a slash command to open, if any.
    #[must_use]
    pub fn take_panel_open_request(&mut self) -> Option<CapabilityPanel> {
        self.panel_commands.take()
    }

    /// Open the capability panel a slash command asked for.
    pub fn open_capability_panel(&mut self, panel: CapabilityPanel) {
        match panel {
            CapabilityPanel::Mcp => self.open_mcp_panel(),
            CapabilityPanel::Plugins => self.open_plugin_panel(),
            CapabilityPanel::Skills | CapabilityPanel::Agents | CapabilityPanel::Hooks => {
                self.open_catalog_panel(panel);
            }
            CapabilityPanel::Settings => self.open_settings_panel(),
            CapabilityPanel::Workflows => self.open_workflows(),
        }
    }

    /// Attach plugin-owned services used by the read-only capability panels.
    ///
    /// Every input is optional so a custom profile still composes and leaves
    /// the corresponding command visible with an explicit unavailable reason.
    pub fn set_capability_services(
        &mut self,
        skills: Option<Arc<heycode_skills::SkillSet>>,
        subagents: Option<Arc<heycode_agent::SubagentRegistry>>,
        hooks: Option<Arc<heycode_hooks::HookService>>,
    ) {
        self.close_capability_catalog();
        self.skills = skills;
        self.subagents = subagents;
        self.hooks = hooks;
        for (panel, attached) in [
            (CapabilityPanel::Skills, self.skills.is_some()),
            (CapabilityPanel::Agents, self.subagents.is_some()),
            (CapabilityPanel::Hooks, self.hooks.is_some()),
        ] {
            if attached {
                self.panel_commands.attach(panel);
            }
        }
    }

    /// Attach the effect-owned background-job registry used by the Jobs side
    /// panel. Absence remains visible rather than becoming an empty registry.
    pub fn set_job_registry(&mut self, jobs: Option<Arc<heycode_agent::JobRegistry>>) {
        self.jobs = jobs;
    }

    /// Currently open side panel.
    #[must_use]
    pub const fn side_panel_kind(&self) -> Option<SidePanelKind> {
        self.side_panel
    }

    /// Current bounded projection for the open side panel.
    #[must_use]
    pub fn side_panel_snapshot(&self) -> Option<SidePanelSnapshot> {
        self.side_panel.map(|panel| match panel {
            SidePanelKind::Diff => diff_snapshot(&self.items),
            SidePanelKind::Jobs => jobs_snapshot(self.jobs.as_deref(), &self.items),
            SidePanelKind::Agents => agents_snapshot(self.subagents.as_deref()),
        })
    }

    fn cycle_side_panel(&mut self) {
        self.side_panel = self
            .side_panel
            .map_or(Some(SidePanelKind::Diff), SidePanelKind::next);
    }

    /// Current skills/agents/hooks catalog panel.
    #[must_use]
    pub const fn capability_catalog(&self) -> Option<&CapabilityCatalogView> {
        self.capability_catalog.as_ref()
    }

    pub(crate) fn skills_panel(&self) -> Option<&crate::skills_panel::SkillsPanel> {
        self.skills_panel.as_ref()
    }

    fn open_catalog_panel(&mut self, panel: CapabilityPanel) {
        if !self.claim_panel_surface() {
            return;
        }
        self.close_capability_catalog();
        if panel == CapabilityPanel::Skills {
            match self.skills.clone() {
                Some(skills) => match crate::skills_panel::SkillsPanel::open(skills) {
                    Ok(panel) => self.skills_panel = Some(panel),
                    Err(error) => self.items.push(Item::Error(error.to_string())),
                },
                None => self.items.push(Item::Error(
                    "Skills are unavailable in this session".to_owned(),
                )),
            }
            return;
        }
        let view = match panel {
            CapabilityPanel::Skills => None,
            CapabilityPanel::Agents => self.subagents.as_deref().map(agents_catalog),
            CapabilityPanel::Hooks => self.hooks.as_deref().map(hooks_catalog),
            CapabilityPanel::Mcp
            | CapabilityPanel::Plugins
            | CapabilityPanel::Settings
            | CapabilityPanel::Workflows => None,
        };
        let Some(view) = view else {
            self.items.push(Item::Error(format!(
                "{} panel is not attached",
                panel.as_str()
            )));
            return;
        };
        self.profile_picker = None;
        self.session_browser = None;
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.close_settings_panel();
        self.capability_catalog = Some(view);
        if panel == CapabilityPanel::Agents
            && let Some(registry) = self.subagents.clone()
        {
            self.agent_readiness =
                Some(crate::agent_readiness::AgentReadinessProbes::new(registry));
            self.update_agent_readiness_view();
        }
    }

    /// Apply completed readiness probes to the actual catalog surface.
    pub fn poll_agent_readiness(&mut self) {
        if self
            .agent_readiness
            .as_mut()
            .is_some_and(crate::agent_readiness::AgentReadinessProbes::poll)
        {
            self.update_agent_readiness_view();
        }
    }

    fn update_agent_readiness_view(&mut self) {
        if let (Some(registry), Some(probes), Some(current)) = (
            &self.subagents,
            &self.agent_readiness,
            &self.capability_catalog,
        ) {
            if current.panel() != CapabilityPanel::Agents {
                return;
            }
            let mut view =
                crate::panel_commands::agents_catalog_with_readiness(registry, probes.states());
            view.preserve_selection(current);
            self.capability_catalog = Some(view);
        }
    }

    /// Close one command's transcript block with the line it produced.
    ///
    /// The renderer pairs this with the echo above it, so the caller supplies
    /// only the settled outcome. A panel opened by keybinding or by a host
    /// request has no echo to close and so gets no receipt, which is what the
    /// source shows too: the line belongs to the command, not to the panel.
    pub(crate) fn push_command_receipt(&mut self, text: impl Into<String>) {
        if !matches!(self.last_drawn_item(), Some(Item::Command(_))) {
            return;
        }
        self.items.push(Item::Info(text.into()));
    }

    /// Newest item the transcript actually draws rows for.
    fn last_drawn_item(&self) -> Option<&Item> {
        self.items.iter().rev().find(|item| !item.renders_no_rows())
    }

    /// Close the current read-only capability catalog.
    pub fn close_capability_catalog(&mut self) {
        self.agent_readiness = None;
        self.capability_catalog = None;
        self.skills_panel = None;
        self.skill_doctor_panel = None;
    }

    fn handle_capability_catalog_key(&mut self, code: crossterm::event::KeyCode) {
        if self.agent_readiness.is_some() {
            if code == crossterm::event::KeyCode::Char('c') {
                if let Some(probes) = &mut self.agent_readiness {
                    probes.cancel();
                }
                self.update_agent_readiness_view();
                return;
            }
            if code == crossterm::event::KeyCode::Char('r') {
                if let Some(registry) = self.subagents.clone() {
                    self.agent_readiness =
                        Some(crate::agent_readiness::AgentReadinessProbes::new(registry));
                }
                self.update_agent_readiness_view();
                return;
            }
        }
        match code {
            crossterm::event::KeyCode::Up => {
                if let Some(panel) = self.capability_catalog.as_mut() {
                    panel.move_selection(-1);
                }
            }
            crossterm::event::KeyCode::Down => {
                if let Some(panel) = self.capability_catalog.as_mut() {
                    panel.move_selection(1);
                }
            }
            crossterm::event::KeyCode::Esc => self.close_capability_catalog(),
            _ => {}
        }
    }

    /// Attach the shared settings/CAS service and settings-surface registry.
    pub fn set_settings_services(
        &mut self,
        settings: Arc<heycode_settings::SettingsService>,
        ui: Arc<heycode_ui::settings_ui::SettingsUiRegistry>,
    ) {
        self.settings_service = Some(settings);
        self.settings_ui = Some(ui);
        self.panel_commands.attach(CapabilityPanel::Settings);
    }

    /// Attach the durable query/lifecycle owner and the live current session.
    pub fn set_session_service(
        &mut self,
        query: Arc<heycode_session::SessionQueryService>,
        current: Arc<std::sync::Mutex<heycode_session::Session>>,
        new_session_metadata: heycode_session::SessionCreationMetadata,
    ) {
        self.current_session_title = current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                heycode_session::SessionEventKind::SessionTitle { title } => Some(title.clone()),
                _ => None,
            });
        self.session_query = Some(query);
        self.current_session = Some(current);
        self.new_session_metadata = Some(new_session_metadata);
    }

    /// Current session browser.
    #[must_use]
    pub const fn session_browser(&self) -> Option<&SessionBrowserView> {
        self.session_browser.as_ref()
    }

    /// Mutable browser state for host/tests applying explicit filter controls.
    #[must_use]
    pub fn session_browser_mut(&mut self) -> Option<&mut SessionBrowserView> {
        self.session_browser.as_mut()
    }

    /// Open the bounded browser from a fresh lower-service page.
    pub fn open_session_browser(&mut self) {
        self.close_capability_catalog();
        if self.workspace_trust.is_some()
            || self.pending_secret.is_some()
            || self.pending_ask.is_some()
            || self.pending_command_confirmation.is_some()
            || self.pending_runtime_question.is_some()
            || self
                .onboarding
                .as_ref()
                .is_some_and(|onboarding| onboarding.active)
        {
            return;
        }
        let Some(query) = self.session_query.clone() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        let Some(current) = self.current_session.as_ref() else {
            self.items
                .push(Item::Error("current session is unavailable".to_owned()));
            return;
        };
        let guard = current.lock().unwrap_or_else(|error| error.into_inner());
        let current_id = guard.id().clone();
        let current_cwd = self.cwd.clone();
        let current_runtime = guard
            .runtime_link()
            .map(|(runtime, _)| runtime.to_owned())
            .or_else(|| {
                guard
                    .metadata()
                    .and_then(|metadata| metadata.runtime().map(str::to_owned))
            });
        drop(guard);
        self.profile_picker = None;
        self.close_settings_panel();
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.command_palette = None;
        self.session_browser = Some(SessionBrowserView::new(
            query,
            current_id,
            current_cwd,
            current_runtime,
        ));
    }

    /// Re-run the current bounded page after a filter or lifecycle commit.
    pub fn refresh_session_browser(&mut self) {
        if let Some(browser) = self.session_browser.as_mut() {
            browser.refresh();
        }
    }

    /// Execute one typed lifecycle request emitted by a slash command.
    pub fn handle_session_command(&mut self, request: SessionCommandRequest) {
        match request {
            SessionCommandRequest::Browse => {
                self.open_session_browser();
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.start_search();
                }
            }
            SessionCommandRequest::RewindPicker(request) => {
                if self.current_session_id().as_ref() != Some(request.session_id()) {
                    self.items.push(Item::Error(
                        "Rewind checkpoint belongs to another session".to_owned(),
                    ));
                } else if !self.high_priority_modal_open() && self.pending_plan_review.is_none() {
                    self.close_low_priority_surfaces();
                    self.rewind_picker = Some(crate::rewind_picker::RewindPicker::new(request));
                }
            }
            SessionCommandRequest::New(title) => self.create_session(title),
            #[cfg(unix)]
            SessionCommandRequest::BackgroundFork(options) => self.fork_background_session(options),
            SessionCommandRequest::Resume(id) => self.select_session(id),
            SessionCommandRequest::ResumeSearch(search) => self.search_sessions(&search),
            SessionCommandRequest::Branch { session_id, title } => {
                self.branch_session(session_id, title)
            }
            SessionCommandRequest::Fork(id) => {
                let id = id.or_else(|| self.current_session_id());
                if let Some(id) = id {
                    self.fork_session(id);
                } else {
                    self.session_error(heycode_session::SessionQueryError::SessionNotFound);
                }
            }
            SessionCommandRequest::Rename(title) => {
                let Some(id) = self.current_session_id() else {
                    self.session_error(heycode_session::SessionQueryError::SessionNotFound);
                    return;
                };
                let title = title.unwrap_or_else(|| {
                    let events = self.current_session.as_ref().map(|session| {
                        let session = session.lock().unwrap_or_else(|error| error.into_inner());
                        heycode_session::SessionTitle::from_conversation(session.events())
                    });
                    events.unwrap_or_else(|| heycode_session::SessionTitle::from_conversation(&[]))
                });
                self.rename_session(id, title);
            }
            SessionCommandRequest::Archive(Some(id)) => {
                self.archive_session(id, heycode_session::SessionArchiveAction::Archive)
            }
            SessionCommandRequest::Archive(None) => self.open_session_browser(),
            SessionCommandRequest::Delete(Some(id)) => {
                if self.session_browser.is_none() {
                    self.open_session_browser();
                }
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.begin_delete(id);
                }
            }
            SessionCommandRequest::Delete(None) => self.open_session_browser(),
            SessionCommandRequest::PlainTextExport { destination } => {
                self.open_plain_text_export(destination)
            }
            SessionCommandRequest::Export { session_id, format } => {
                let id = session_id.or_else(|| self.current_session_id());
                if let Some(id) = id {
                    self.export_session(id, format);
                } else {
                    self.session_error(heycode_session::SessionQueryError::SessionNotFound);
                }
            }
        }
    }

    pub(crate) fn current_session_id(&self) -> Option<heycode_core::SessionId> {
        self.current_session.as_ref().map(|session| {
            session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .id()
                .clone()
        })
    }

    fn create_session(&mut self, title: Option<heycode_session::SessionTitle>) {
        let (Some(query), Some(metadata)) = (
            self.session_query.as_ref(),
            self.new_session_metadata.as_ref(),
        ) else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        let command = self.items.last().and_then(|item| match item {
            Item::Command(command)
                if matches!(
                    command.split_whitespace().next(),
                    Some("/clear" | "/reset" | "/new")
                ) =>
            {
                Some(command.clone())
            }
            _ => None,
        });
        let title = title.or_else(|| {
            matches!(command.as_deref(), Some("/clear" | "/reset"))
                .then(|| self.current_session_title.as_deref())
                .flatten()
                .and_then(|title| heycode_session::SessionTitle::new(title).ok())
        });
        let mut request = heycode_session::SessionCreateRequest::new(metadata.clone());
        if let Some(title) = title {
            request = request.with_title(title);
        }
        match query.create(&request) {
            Ok(session) => {
                let session_id = session.id().clone();
                drop(session);
                self.run_outcome =
                    Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Created {
                        session_id,
                        command,
                    }));
            }
            Err(error) => self.session_error(error),
        }
    }

    fn select_session(&mut self, id: heycode_core::SessionId) {
        if self.current_session_id().as_ref() == Some(&id) {
            self.session_browser = None;
            self.items
                .push(Item::Info("session is already current".to_owned()));
            return;
        }
        #[cfg(unix)]
        if self.resume_background_session(&id) {
            return;
        }
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        match query.resume(&id) {
            Ok(session) => {
                drop(session);
                self.run_outcome =
                    Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Resume {
                        session_id: id,
                    }));
            }
            Err(error) => self.session_error(error),
        }
    }

    fn fork_session(&mut self, parent_session_id: heycode_core::SessionId) {
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        let parent_title = query.resume(&parent_session_id).ok().and_then(|source| {
            source
                .events()
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    heycode_session::SessionEventKind::SessionTitle { title } => {
                        Some(title.clone())
                    }
                    _ => None,
                })
        });
        match query.fork(&parent_session_id, heycode_session::ForkBoundary::Latest) {
            Ok(session) => {
                let session_id = session.id().clone();
                drop(session);
                let command = format!("/branch --session {parent_session_id}");
                self.run_outcome =
                    Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Forked {
                        parent_session_id,
                        parent_title,
                        session_id,
                        title: None,
                        command,
                    }));
            }
            Err(error) => self.session_error(error),
        }
    }

    fn rename_session(
        &mut self,
        id: heycode_core::SessionId,
        title: heycode_session::SessionTitle,
    ) {
        let Some(query) = self.session_query.clone() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        let result = if self.current_session_id().as_ref() == Some(&id) {
            let Some(current) = self.current_session.as_ref() else {
                self.session_error(heycode_session::SessionQueryError::SessionNotFound);
                return;
            };
            let mut session = current.lock().unwrap_or_else(|error| error.into_inner());
            query.rename_open(&mut session, &title)
        } else {
            query.rename(&id, &title)
        };
        match result {
            Ok(summary) => {
                let title = summary.title().unwrap_or(title.as_str());
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.set_notice(format!("Renamed to {title}"));
                    browser.refresh();
                }
                self.items.push(Item::Info(format!("Renamed to {title}")));
            }
            Err(error) => self.session_error(error),
        }
    }

    fn archive_session(
        &mut self,
        id: heycode_core::SessionId,
        action: heycode_session::SessionArchiveAction,
    ) {
        if self.current_session_id().as_ref() == Some(&id) {
            self.session_error(heycode_session::SessionQueryError::CurrentSession);
            return;
        }
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        match query.archive(&id, action) {
            Ok(_) => {
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.set_notice("session archive state committed".to_owned());
                    browser.refresh();
                }
            }
            Err(error) => self.session_error(error),
        }
    }

    fn delete_session(&mut self, id: heycode_core::SessionId) {
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        let mut request = heycode_session::SessionDeleteRequest::new(id);
        if let Some(current) = self.current_session_id() {
            request = request.with_current(current);
        }
        match query.delete(&request) {
            Ok(receipt) => {
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.set_notice(format!(
                        "session moved to recoverable trash; recovery {}",
                        receipt.recovery_id().as_str()
                    ));
                    browser.refresh();
                }
            }
            Err(error) => self.session_error(error),
        }
    }

    fn export_session(
        &mut self,
        id: heycode_core::SessionId,
        format: heycode_session::SessionExportFormat,
    ) {
        let flush_failed = self.current_session_id().as_ref() == Some(&id)
            && self.current_session.as_ref().is_some_and(|current| {
                current
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .flush()
                    .is_err()
            });
        if flush_failed {
            self.session_error(heycode_session::SessionQueryError::ExportFailed);
            return;
        }
        let Some(query) = self.session_query.as_ref() else {
            self.items
                .push(Item::Error("session lifecycle is unavailable".to_owned()));
            return;
        };
        match query.export(&id, format) {
            Ok(receipt) => {
                let notice = format!(
                    "session export committed: {} ({} bytes)",
                    safe_path_display(receipt.path()),
                    receipt.byte_len()
                );
                if let Some(browser) = self.session_browser.as_mut() {
                    browser.set_notice(notice.clone());
                }
                self.items.push(Item::Info(notice));
            }
            Err(error) => self.session_error(error),
        }
    }

    fn session_error(&mut self, error: heycode_session::SessionQueryError) {
        if let Some(browser) = self.session_browser.as_mut() {
            browser.set_notice(error.to_string());
        }
        self.items.push(Item::Error(error.to_string()));
    }

    fn handle_session_browser_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        let action = self
            .session_browser
            .as_mut()
            .map_or(SessionBrowserAction::None, |browser| {
                browser.handle_key(code, modifiers)
            });
        match action {
            SessionBrowserAction::None => {}
            SessionBrowserAction::Refresh => self.refresh_session_browser(),
            SessionBrowserAction::Resume(id) => self.select_session(id),
            SessionBrowserAction::Fork(id) => self.fork_session(id),
            SessionBrowserAction::New => self.create_session(None),
            SessionBrowserAction::Rename(id, title) => self.rename_session(id, title),
            SessionBrowserAction::ToggleArchive(id) => {
                let action = self
                    .session_browser
                    .as_ref()
                    .and_then(|browser| browser.rows().iter().find(|row| row.summary().id() == &id))
                    .map_or(heycode_session::SessionArchiveAction::Archive, |row| {
                        if row.summary().storage() == heycode_session::SessionStorageState::Archived
                        {
                            heycode_session::SessionArchiveAction::Restore
                        } else {
                            heycode_session::SessionArchiveAction::Archive
                        }
                    });
                self.archive_session(id, action);
            }
            SessionBrowserAction::Delete(id) => self.delete_session(id),
            SessionBrowserAction::Export(id, format) => self.export_session(id, format),
            SessionBrowserAction::Close => self.session_browser = None,
        }
    }

    /// Current settings browser.
    #[must_use]
    pub fn settings_panel(&self) -> Option<&SettingsPanelView> {
        self.settings_panel.as_ref().map(SettingsShellView::config)
    }

    pub(crate) fn settings_shell(&self) -> Option<&SettingsShellView> {
        self.settings_panel.as_ref()
    }

    /// Open the settings browser from fresh authoritative snapshots.
    pub fn open_settings_panel(&mut self) {
        use heycode_agent::ui::{SettingsShellSection, SettingsShellSnapshot, SettingsShellTab};
        let unavailable = || SettingsShellSection::Unavailable {
            reason: "Open /status or /usage to load the current session snapshot.".to_owned(),
        };
        self.open_settings_shell(
            SettingsShellTab::Config,
            SettingsShellSnapshot {
                status: unavailable(),
                usage: unavailable(),
                stats: SettingsShellSection::Unavailable {
                    reason: "Historical statistics are unavailable in this build.".to_owned(),
                },
                stats_snapshot: None,
            },
        );
    }

    fn open_settings_shell(
        &mut self,
        tab: heycode_agent::ui::SettingsShellTab,
        snapshot: heycode_agent::ui::SettingsShellSnapshot,
    ) {
        if !self.claim_panel_surface() {
            return;
        }
        let (Some(settings), Some(ui)) = (self.settings_service.clone(), self.settings_ui.clone())
        else {
            self.items
                .push(Item::Error("settings browser is not attached".to_owned()));
            return;
        };
        match SettingsShellView::open(settings, ui, tab, snapshot) {
            Ok(panel) => self.settings_panel = Some(panel),
            Err(error) => self.items.push(Item::Error(error)),
        }
    }

    /// Close the settings browser.
    pub fn close_settings_panel(&mut self) {
        self.settings_panel = None;
    }

    fn handle_settings_panel_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        let outcome = match self.settings_panel.as_mut() {
            Some(panel) => panel.handle_key(code, modifiers),
            None => return,
        };
        if outcome == SettingsPanelKeyOutcome::Close {
            self.close_settings_panel();
        }
    }

    fn handle_theme_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                let original = self
                    .theme_picker
                    .as_ref()
                    .and_then(|picker| picker.themes.get(picker.original).cloned());
                self.theme_picker = None;
                if let Some(theme) = original {
                    self.apply_selected_theme(&theme);
                }
            }
            KeyCode::Up => {
                if let Some(picker) = self.theme_picker.as_mut()
                    && !picker.themes.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.themes.len() - 1);
                }
                self.preview_theme_selection();
            }
            KeyCode::Down | KeyCode::Tab => {
                if let Some(picker) = self.theme_picker.as_mut()
                    && !picker.themes.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.themes.len();
                }
                self.preview_theme_selection();
            }
            KeyCode::Enter => {
                let selection = self.theme_picker.as_ref().and_then(|picker| {
                    picker
                        .themes
                        .get(picker.selected)
                        .cloned()
                        .map(|theme| (theme, picker.revision))
                });
                let Some((theme, revision)) = selection else {
                    return;
                };
                let Some(settings) = self.settings_service.clone() else {
                    self.items
                        .push(Item::Error("settings service is unavailable".to_owned()));
                    return;
                };
                match heycode_ui::preferences::SettingsBackedUiPreferences::new(settings)
                    .store_theme(theme.id().as_str(), revision)
                {
                    Ok(_) => {
                        self.theme_picker = None;
                        self.apply_selected_theme(&theme);
                        self.items
                            .push(Item::Info(crate::panel_frame::theme_receipt(
                                theme.id().as_str(),
                            )));
                    }
                    Err(error) => self.items.push(Item::Error(error.to_string())),
                }
            }
            _ => {}
        }
    }

    fn handle_keymap_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => self.keymap_picker = None,
            KeyCode::Up => {
                if let Some(picker) = self.keymap_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.rows.len() - 1);
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if let Some(picker) = self.keymap_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.rows.len();
                }
            }
            KeyCode::Enter => {
                let action = self
                    .keymap_picker
                    .as_ref()
                    .and_then(|picker| picker.rows.get(picker.selected).map(|(action, _)| *action));
                if let Some(action) = action {
                    self.replace_input(format!("/keymap {} ", action.as_str()));
                    self.keymap_picker = None;
                    self.help_panel = None;
                    self.memory_panel = None;
                    self.skill_doctor_panel = None;
                    self.copy_panel = None;
                }
            }
            _ => {}
        }
    }

    fn handle_scroll_speed_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                // The reference reports the outcome of a cancelled picker
                // rather than closing in silence, so the transcript still
                // says what the command did.
                self.scroll_speed_picker = None;
                self.items.push(Item::Info(
                    crate::panel_frame::SCROLL_SPEED_UNCHANGED.to_owned(),
                ));
            }
            KeyCode::Left | KeyCode::Down => {
                if let Some(picker) = self.scroll_speed_picker.as_mut() {
                    picker.quarters = picker.quarters.saturating_sub(1).max(1);
                }
            }
            KeyCode::Right | KeyCode::Up | KeyCode::Tab => {
                if let Some(picker) = self.scroll_speed_picker.as_mut() {
                    picker.quarters = picker.quarters.saturating_add(1).min(40);
                }
            }
            KeyCode::Char('r') => {
                if let Some(picker) = self.scroll_speed_picker.as_mut() {
                    picker.quarters = 4;
                }
            }
            KeyCode::Enter => {
                let Some((quarters, revision)) = self
                    .scroll_speed_picker
                    .as_ref()
                    .map(|picker| (picker.quarters, picker.revision))
                else {
                    return;
                };
                let Some(settings) = self.settings_service.clone() else {
                    self.items
                        .push(Item::Error("settings service is unavailable".to_owned()));
                    return;
                };
                match heycode_ui::preferences::SettingsBackedUiPreferences::new(settings)
                    .store_scroll_speed(f32::from(quarters) / 4.0, revision)
                {
                    Ok(committed) => {
                        self.scroll_speed_picker = None;
                        self.set_scroll_speed(committed.preferences.scroll_speed());
                        self.items
                            .push(Item::Info(crate::panel_frame::scroll_speed_receipt(
                                &format_scroll_speed(committed.preferences.scroll_speed()),
                            )));
                    }
                    Err(error) => self.items.push(Item::Error(error.to_string())),
                }
            }
            _ => {}
        }
    }

    fn preview_scroll_speed(&mut self, direction: i16) {
        let Some(picker) = self.scroll_speed_picker.as_mut() else {
            return;
        };
        let total = picker
            .preview_remainder
            .saturating_add(direction.saturating_mul(i16::from(picker.quarters) * 3));
        let rows = total / 4;
        picker.preview_remainder = total % 4;
        picker.preview_offset = picker.preview_offset.saturating_add(i32::from(rows));
    }

    fn scroll_transcript_by_wheel(&mut self, direction: i16) {
        let total = self
            .scroll_wheel_remainder
            .saturating_add(direction.saturating_mul(i16::from(self.scroll_speed_quarters) * 3));
        let rows = total / 4;
        self.scroll_wheel_remainder = total % 4;
        if rows >= 0 {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(usize::try_from(rows).unwrap_or(usize::MAX));
        } else {
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_sub(usize::try_from(-rows).unwrap_or(usize::MAX));
        }
    }

    /// Adopt a detected capability tier and the theme to render it with.
    ///
    /// Resolving here rather than at each draw is what makes "a terminal that
    /// cannot do 24-bit is never handed 24-bit" a property of one call: the
    /// renderer only ever sees already-resolved colours.
    pub fn apply_terminal(
        &mut self,
        capabilities: heycode_ui::terminal::TerminalCapabilities,
        theme: &heycode_ui::theme::Theme,
    ) {
        self.styles = crate::terminal::Styles::new(&theme.resolve(capabilities.color()));
        // Capabilities decide what may be DRAWN; what may be asked of the
        // terminal's input was settled when the screen was entered.
        self.chrome =
            crate::terminal::Chrome::new(capabilities.render()).with_input(self.chrome.input());
        self.terminal_capabilities = Some(capabilities);
        self.transcript_style_generation = self.transcript_style_generation.saturating_add(1);
    }

    /// Role colours for the active theme and tier.
    #[must_use]
    pub const fn styles(&self) -> crate::terminal::Styles {
        self.styles
    }

    /// Decoration the active terminal permits.
    #[must_use]
    pub const fn chrome(&self) -> crate::terminal::Chrome {
        self.chrome
    }

    /// Persistent shell header/footer choices consumed by both layout and renderers.
    #[must_use]
    pub const fn shell_preferences(&self) -> heycode_ui::preferences::ShellChromePreferences {
        self.shell_preferences
    }

    /// Apply one validated Settings generation to the live shell.
    pub fn set_shell_preferences(
        &mut self,
        preferences: heycode_ui::preferences::ShellChromePreferences,
    ) {
        self.shell_preferences = preferences;
    }

    /// Record what the entered screen asked of this terminal's input.
    pub fn set_input_protocol(&mut self, input: crate::terminal::InputProtocol) {
        self.chrome = self.chrome.with_input(input);
    }

    /// Install a resolved keymap, replacing the shipped defaults.
    pub fn set_keymap(&mut self, keymap: heycode_ui::keymap::Keymap) {
        self.keymap = keymap;
    }

    /// Attach CMD09/CMD10's private human-control inbox.
    pub fn set_human_commands(&mut self, bridge: crate::human_commands::HumanCommandBridge) {
        bridge.attach();
        self.human_commands = bridge;
    }

    /// Apply one pending human-only command request.
    pub fn poll_human_command(&mut self) {
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && let Some(panel) = self.advisor_bridge.take()
        {
            self.close_low_priority_surfaces();
            self.advisor_panel = Some(panel);
        }
        let Some(request) = self.human_commands.take() else {
            return;
        };
        use crate::human_commands::HumanCommandRequest;
        match request {
            HumanCommandRequest::OpenDiff => {
                if !self.high_priority_modal_open() {
                    self.close_low_priority_surfaces();
                    // An empty framed panel says nothing. The reference answers
                    // a `/diff` it cannot fill with a receipt that names what
                    // the panel would have shown.
                    if crate::side_panel::diff_snapshot(&self.items)
                        .rows()
                        .is_empty()
                    {
                        self.items.push(Item::Info(
                            "The diff panel shows file edits recorded in this session — \
                             no edit or write has produced a diff yet"
                                .to_owned(),
                        ));
                    } else {
                        self.side_panel = Some(SidePanelKind::Diff);
                    }
                }
            }
            HumanCommandRequest::CopyAnswer { latest_index } => self.copy_answer(latest_index),
            HumanCommandRequest::CopyArgumentError { message } => {
                if !self.high_priority_modal_open() {
                    let message = if self.completed_copy_answers().is_empty() {
                        "No assistant message to copy".to_owned()
                    } else {
                        message
                    };
                    self.items.push(Item::Error(message));
                }
            }
            HumanCommandRequest::Mention(reference) => self.insert_mention(reference),
            HumanCommandRequest::OpenTheme {
                themes,
                selected_id,
                revision,
            } => self.open_theme_picker(themes, &selected_id, revision),
            HumanCommandRequest::OpenKeymap { keymap, revision } => {
                self.open_keymap_picker(keymap, revision);
            }
            HumanCommandRequest::OpenScrollSpeed { quarters, revision } => {
                self.open_scroll_speed_picker(quarters, revision);
            }
            HumanCommandRequest::ApplyTheme(theme) => {
                self.apply_selected_theme(&theme);
                self.items.push(Item::Info(format!(
                    "theme persisted as {}",
                    theme.id().as_str()
                )));
            }
            HumanCommandRequest::ApplyVim(enabled) => {
                self.set_vim_mode(enabled);
                self.items.push(Item::Info(format!(
                    "composer Vim mode {}",
                    if enabled { "enabled" } else { "disabled" }
                )));
            }
            HumanCommandRequest::ApplyShell(preferences) => {
                self.set_shell_preferences(preferences);
            }
            HumanCommandRequest::ApplyScrollSpeed { quarters } => {
                self.set_scroll_speed(f32::from(quarters) / 4.0);
            }
            HumanCommandRequest::ApplyKeymap(keymap) => {
                self.set_keymap(keymap);
                self.items
                    .push(Item::Info("keymap persisted and applied live".to_owned()));
            }
            HumanCommandRequest::ApplyFocus(enabled) => {
                self.focus_view = enabled;
                self.scroll_from_bottom = 0;
                self.reasoning_reveal = None;
                self.reasoning_focus = None;
                self.close_low_priority_surfaces();
                self.items.push(Item::Info(format!(
                    "Focus view {}",
                    if self.focus_view {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )));
            }
            HumanCommandRequest::ApplyPromptColor(color) => {
                self.prompt_color = color;
                self.items.push(Item::Info(color.map_or_else(
                    || "Session color reset to default".to_owned(),
                    |color| format!("Session color set to: {}", color.as_str()),
                )));
            }
        }
    }

    /// Whether the transcript is projected into its compact current-turn view.
    #[must_use]
    pub const fn focus_view(&self) -> bool {
        self.focus_view
    }

    /// Current per-session prompt-bar accent selection.
    #[must_use]
    pub const fn prompt_color(&self) -> Option<crate::human_commands::PromptColor> {
        self.prompt_color
    }

    /// Latest durable current-session title, when one has been named.
    #[must_use]
    pub fn current_session_title(&self) -> Option<&str> {
        self.current_session_title.as_deref()
    }

    /// Current theme preview.
    #[must_use]
    pub const fn theme_picker(&self) -> Option<&ThemePickerView> {
        self.theme_picker.as_ref()
    }

    /// Current shortcut browser.
    #[must_use]
    pub const fn keymap_picker(&self) -> Option<&KeymapPickerView> {
        self.keymap_picker.as_ref()
    }

    /// Current mouse-wheel speed picker.
    #[must_use]
    pub const fn scroll_speed_picker(&self) -> Option<&ScrollSpeedPickerView> {
        self.scroll_speed_picker.as_ref()
    }

    /// Active mouse-wheel multiplier.
    #[must_use]
    pub const fn scroll_speed(&self) -> f32 {
        self.scroll_speed_quarters as f32 / 4.0
    }

    /// Apply one validated Settings generation to transcript wheel behavior.
    pub fn set_scroll_speed(&mut self, speed: f32) {
        let quarters = (speed * 4.0) as u8;
        self.scroll_speed_quarters = quarters.clamp(1, 40);
        self.scroll_wheel_remainder = 0;
    }

    /// Whether Vim behavior is enabled for the composer.
    #[must_use]
    pub const fn vim_enabled(&self) -> bool {
        self.vim_enabled
    }

    /// Whether an enabled Vim composer is in insert state.
    #[must_use]
    pub const fn vim_insert(&self) -> bool {
        self.vim_insert
    }

    pub(crate) fn show_copy_notice(&mut self, message: String) {
        self.copy_notice = Some((
            message,
            std::time::Instant::now() + std::time::Duration::from_millis(2500),
        ));
    }

    pub(crate) fn copy_notice_text(&self) -> Option<&str> {
        self.copy_notice
            .as_ref()
            .filter(|(_, until)| *until > std::time::Instant::now())
            .map(|(text, _)| text.as_str())
    }

    /// Take one explicit clipboard request. The terminal adapter decides
    /// whether the current presentation can emit OSC 52.
    #[must_use]
    pub fn take_clipboard_request(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    pub(crate) fn recomposition_blocked(&self) -> bool {
        self.active_turn
            || !self.input.lines().join("\n").is_empty()
            || !self.pending_attachments.is_empty()
            || self.pending_send.is_some()
            || self.side_command_activity.is_some()
            || !self.queued_commands.is_empty()
            || self.high_priority_modal_open()
            || self.run_outcome.is_some()
    }

    fn high_priority_modal_open(&self) -> bool {
        self.add_directory_dialog.is_some()
            || self.export_progress.is_some()
            || self.standard_priority_modal_open()
    }

    fn standard_priority_modal_open(&self) -> bool {
        self.workspace_trust.is_some()
            || self.pending_secret.is_some()
            || self.pending_ask.is_some()
            || self.pending_runtime_question.is_some()
            || self.pending_mcp_elicitation.is_some()
            || self.pending_command_confirmation.is_some()
            || self
                .onboarding
                .as_ref()
                .is_some_and(|onboarding| onboarding.active)
    }

    fn close_low_priority_surfaces(&mut self) {
        self.sandbox_panel = None;
        self.autocompact_panel = None;
        self.export_panel = None;
        self.close_workflows();
        self.close_tasks();
        self.profile_picker = None;
        self.session_browser = None;
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.close_settings_panel();
        self.close_capability_catalog();
        self.theme_picker = None;
        self.advisor_panel = None;
        self.rewind_picker = None;
        self.keymap_picker = None;
        self.help_panel = None;
        self.memory_panel = None;
        self.skill_doctor_panel = None;
        self.copy_panel = None;
        self.scroll_speed_picker = None;
    }

    /// Claim the single panel surface, or refuse.
    ///
    /// The renderer (`crate::render::draw`), the key router
    /// (`handle_terminal_event`) and the screen-reader projection each walk
    /// their own ordered cascade and stop at the first open surface. Two open
    /// surfaces therefore mean the frame shows one panel while every keystroke
    /// drives another, so a panel is opened only by closing every other
    /// low-priority surface first — derived here once instead of restated in
    /// each opener.
    fn claim_panel_surface(&mut self) -> bool {
        if self.high_priority_modal_open() {
            return false;
        }
        self.close_low_priority_surfaces();
        true
    }

    fn open_plain_text_export(&mut self, destination: Option<String>) {
        if self.high_priority_modal_open() {
            return;
        }
        let transcript = match crate::render::conversation_export_text(self) {
            Ok(text) => Arc::<str>::from(text),
            Err(error) => {
                self.items.push(Item::Error(error));
                return;
            }
        };
        if let Some(destination) = destination {
            match crate::export_panel::PlainTextExportRequest::new(&destination, transcript) {
                Ok(request) => self.pending_plain_text_export = Some(request),
                Err(error) => self.items.push(Item::Error(format!(
                    "Failed to export conversation: {error}"
                ))),
            }
        } else {
            self.close_low_priority_surfaces();
            let filename = format!(
                "{}-heycode-conversation.txt",
                chrono::Local::now().format("%Y-%m-%d-%H%M%S")
            );
            match crate::export_panel::ConversationExportPanel::new(transcript, filename) {
                Ok(panel) => self.export_panel = Some(panel),
                Err(error) => self.items.push(Item::Error(format!(
                    "Failed to export conversation: {error}"
                ))),
            }
        }
    }

    fn apply_export_action(&mut self, action: crate::export_panel::ExportAction) {
        use crate::export_panel::ExportAction;
        match action {
            ExportAction::None => return,
            ExportAction::Cancel => self.items.push(Item::Info("Export cancelled".to_owned())),
            ExportAction::Copy { transcript } => {
                self.pending_clipboard = Some(transcript.to_string());
                self.export_clipboard = true;
            }
            ExportAction::Save(request) => self.pending_plain_text_export = Some(request),
        }
        self.export_panel = None;
    }

    fn completed_copy_answers(&self) -> Vec<&str> {
        let live = self
            .live_assistant_item
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        self.items
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, item)| match item {
                Item::Assistant(text) if !live.contains(&index) => Some(text.as_str()),
                _ => None,
            })
            .take(20)
            .collect()
    }

    fn copy_answer(&mut self, latest_index: usize) {
        if self.high_priority_modal_open() {
            return;
        }
        let answers = self.completed_copy_answers();
        let count = answers.len();
        let answer = answers.get(latest_index).map(|text| (*text).to_owned());
        match answer {
            Some(answer) => {
                let always_full = self.settings_service.clone().map(|settings| {
                    heycode_ui::preferences::SettingsBackedUiPreferences::new(settings).load()
                });
                let always_full = match always_full {
                    Some(Ok(preferences)) => preferences.preferences.copy_full_response(),
                    Some(Err(error)) => {
                        self.items
                            .push(Item::Error(format!("Copy preference unavailable: {error}")));
                        false
                    }
                    None => false,
                };
                if !always_full {
                    match crate::copy_panel::CopyPanel::new(answer.clone()) {
                        Ok(Some(panel)) => {
                            self.close_low_priority_surfaces();
                            self.copy_panel = Some(panel);
                            return;
                        }
                        Err(error) => {
                            self.items.push(Item::Error(error.to_string()));
                            return;
                        }
                        Ok(None) => {}
                    }
                }
                match crate::copy_panel::CopySelection::full_response(answer) {
                    Ok(selection) => self.queue_copy_selection(selection, true),
                    Err(error) => self.items.push(Item::Error(error.to_string())),
                }
            }
            None => self.items.push(Item::Error(if count == 0 {
                "No assistant message to copy".to_owned()
            } else {
                format!(
                    "Only {count} assistant {} available to copy",
                    if count == 1 { "message" } else { "messages" }
                )
            })),
        }
    }

    fn queue_copy_selection(
        &mut self,
        selection: crate::copy_panel::CopySelection,
        clipboard: bool,
    ) {
        if clipboard {
            self.pending_clipboard = Some(selection.text().to_owned());
        }
        self.pending_copy_recovery = Some((selection, clipboard));
    }

    fn apply_copy_action(&mut self, action: crate::copy_panel::CopyAction) {
        use crate::copy_panel::CopyAction;
        match action {
            CopyAction::None => {}
            CopyAction::Cancel => {
                self.copy_panel = None;
                self.items.push(Item::Info("Copy cancelled".to_owned()));
            }
            CopyAction::Copy {
                selection,
                always_full,
            } => {
                self.copy_panel = None;
                if always_full {
                    let result = self
                        .settings_service
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("UI preferences are unavailable"))
                        .and_then(|settings| {
                            let store =
                                heycode_ui::preferences::SettingsBackedUiPreferences::new(settings);
                            let current = store.load()?;
                            store.store(
                                &current.preferences.with_copy_full_response(true),
                                current.revision,
                            )?;
                            Ok(())
                        });
                    match result {
                        Ok(()) => self.items.push(Item::Info("Always copy full response saved. Change ui-preferences.copy_full_response in /config to show the picker again.".to_owned())),
                        Err(error) => self.items.push(Item::Error(format!("Copy preference was not saved: {error}"))),
                    }
                }
                self.queue_copy_selection(selection, true);
            }
            CopyAction::Write { selection } => {
                self.copy_panel = None;
                self.queue_copy_selection(selection, false);
            }
        }
    }

    fn insert_mention(&mut self, reference: Option<String>) {
        if self.high_priority_modal_open() {
            return;
        }
        let mention = reference.map_or_else(|| "@".to_owned(), |value| format!("@{value}"));
        if !self.input.lines().iter().all(String::is_empty) {
            let _ = self.input.insert_str(" ");
        }
        let _ = self.input.insert_str(mention);
    }

    fn open_theme_picker(
        &mut self,
        themes: Vec<heycode_ui::theme::Theme>,
        selected_id: &str,
        revision: u64,
    ) {
        if self.high_priority_modal_open() || themes.is_empty() {
            return;
        }
        self.close_low_priority_surfaces();
        let selected = themes
            .iter()
            .position(|theme| theme.id().as_str() == selected_id)
            .unwrap_or(0);
        self.theme_picker = Some(ThemePickerView {
            themes,
            selected,
            original: selected,
            revision,
        });
        self.preview_theme_selection();
    }

    fn open_keymap_picker(&mut self, keymap: heycode_ui::keymap::Keymap, revision: u64) {
        if self.high_priority_modal_open() {
            return;
        }
        self.close_low_priority_surfaces();
        let rows = heycode_ui::keymap::KeymapAction::ALL
            .into_iter()
            .map(|action| (action, keymap.chord(action)))
            .collect();
        self.keymap_picker = Some(KeymapPickerView {
            rows,
            selected: 0,
            revision,
        });
    }

    fn open_scroll_speed_picker(&mut self, quarters: u8, revision: u64) {
        if self.high_priority_modal_open() {
            return;
        }
        self.close_low_priority_surfaces();
        self.scroll_speed_picker = Some(ScrollSpeedPickerView {
            quarters: quarters.clamp(1, 40),
            revision,
            preview_offset: 0,
            preview_remainder: 0,
        });
    }

    fn preview_theme_selection(&mut self) {
        let theme = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.themes.get(picker.selected).cloned());
        if let Some(theme) = theme {
            self.apply_selected_theme(&theme);
        }
    }

    fn apply_selected_theme(&mut self, theme: &heycode_ui::theme::Theme) {
        if let Some(capabilities) = self.terminal_capabilities {
            self.apply_terminal(capabilities, theme);
        }
    }

    /// Apply the composer mode selected by startup preferences or `/vim`.
    pub fn set_vim_mode(&mut self, enabled: bool) {
        self.vim_enabled = enabled;
        self.vim_insert = true;
    }

    /// The live key bindings.
    #[must_use]
    pub const fn keymap(&self) -> &heycode_ui::keymap::Keymap {
        &self.keymap
    }

    /// Cached workspace Git and current-PR facts.
    #[must_use]
    pub const fn workspace_context(&self) -> &crate::workspace_context::WorkspaceContextState {
        &self.workspace_context
    }

    /// Publish the latest cached workspace probe result.
    pub fn set_workspace_context(
        &mut self,
        context: crate::workspace_context::WorkspaceContextState,
    ) {
        self.workspace_context = context;
    }

    /// Cumulative bounded-render work counters for performance diagnostics.
    #[must_use]
    pub fn transcript_cache_metrics(&self) -> crate::transcript::TranscriptCacheMetrics {
        self.transcript_cache.metrics()
    }

    pub(crate) const fn transcript_style_generation(&self) -> u64 {
        self.transcript_style_generation
    }

    /// Current palette view for rendering/testing.
    #[must_use]
    pub fn command_palette(&self) -> Option<&CommandPaletteView> {
        self.command_palette.as_ref()
    }

    /// Current combined route-picker view.
    #[must_use]
    pub fn route_picker(&self) -> Option<&RoutePickerView> {
        self.route_picker.as_ref()
    }

    /// Current effective sandbox capability picker.
    #[must_use]
    pub fn permission_picker(&self) -> Option<&PermissionPickerView> {
        self.permission_picker.as_ref()
    }

    /// Attach the shared MCP10 operations layer plus the live registry.
    ///
    /// The panel never builds its own store or its own management: it drives
    /// the one the composition root already owns. The registry is optional
    /// because a world may manage definitions without connecting to servers,
    /// and the panel says so rather than implying an idle server.
    pub fn set_mcp_services(
        &mut self,
        management: Arc<heycode_mcp::management::McpManagement>,
        registry: Option<Arc<heycode_mcp::McpRegistry>>,
    ) {
        self.panel_commands.attach_mcp(Arc::clone(&management));
        self.mcp_management = Some(management);
        self.mcp_registry = registry;
        // The bridge receives this same concrete owner, so `/mcp` cannot
        // advertise direct operations over a marker with no management layer.
    }

    /// Attach the exact live product reconnect owner, without constructing a fallback.
    pub fn set_mcp_runtime_control(&mut self, control: Arc<heycode_mcp::McpRuntimeControl>) {
        self.panel_commands
            .attach_mcp_runtime_control(control.clone());
        self.mcp_runtime_control = Some(control);
    }

    /// Attach the shared PL06 lifecycle layer plus already-projected package
    /// facts.
    ///
    /// The panel never opens the package cache: provenance and permissions are
    /// filesystem reads, and the index is built by whoever owns the cache. An
    /// absent index makes those sections report themselves unknown, which is a
    /// different statement from "declares none".
    pub fn set_plugin_services(
        &mut self,
        lifecycle: Arc<heycode_extensions::lifecycle::PluginLifecycle>,
        packages: Option<Arc<PluginPackageIndex>>,
    ) {
        self.plugin_lifecycle = Some(lifecycle);
        self.plugin_packages = packages;
        self.panel_commands.attach(CapabilityPanel::Plugins);
    }

    /// Current plugin panel view.
    #[must_use]
    pub fn plugin_panel(&self) -> Option<&PluginPanelView> {
        self.plugin_panel.as_ref()
    }

    /// Open the plugin panel over the current lifecycle state.
    pub fn open_plugin_panel(&mut self) {
        if !self.claim_panel_surface() {
            return;
        }
        match self.list_plugin_rows() {
            Ok(rows) => self.plugin_panel = Some(PluginPanelView::new(rows)),
            Err(error) => self.items.push(Item::Error(error)),
        }
    }

    fn list_plugin_rows(&self) -> Result<Vec<PluginPanelRow>, String> {
        let Some(lifecycle) = self.plugin_lifecycle.as_ref() else {
            return Err("plugin lifecycle is not attached".to_owned());
        };
        let states = match list_plugins(lifecycle)? {
            PluginPanelOutcome::Listed(states) => states,
            // `list` answers `Listed`; anything else is a lifecycle-layer
            // contract break, reported rather than absorbed.
            other => return Err(format!("unexpected plugin list outcome: {other:?}")),
        };
        Ok(build_plugin_rows(&states, self.plugin_packages.as_deref()))
    }

    fn refresh_plugin_rows(&mut self, notice: Option<String>) {
        let rows = self.list_plugin_rows();
        let Some(panel) = self.plugin_panel.as_mut() else {
            return;
        };
        match rows {
            Ok(rows) => {
                panel.set_rows(rows);
                if let Some(notice) = notice {
                    panel.note_success(notice);
                }
            }
            Err(error) => panel.note_failure(error),
        }
    }

    fn handle_plugin_panel_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        let outcome = match self.plugin_panel.as_mut() {
            Some(panel) => panel.handle_key(code, modifiers),
            None => return,
        };
        match outcome {
            PluginPanelKeyOutcome::Handled => {}
            PluginPanelKeyOutcome::Close => self.close_plugin_panel(),
            PluginPanelKeyOutcome::Run(intent) => self.run_plugin_intent(&intent),
        }
    }

    fn run_plugin_intent(&mut self, intent: &PluginPanelIntent) {
        let Some(lifecycle) = self.plugin_lifecycle.clone() else {
            if let Some(panel) = self.plugin_panel.as_mut() {
                panel.note_failure("plugin lifecycle is not attached");
            }
            return;
        };
        match dispatch_plugin(&lifecycle, intent) {
            // Rows are republished only from a fresh read after the store
            // committed, never from the operation's own optimism.
            Ok(PluginPanelOutcome::Listed(_)) => {
                self.refresh_plugin_rows(Some("refreshed".to_owned()));
            }
            Ok(PluginPanelOutcome::Committed(message)) => self.refresh_plugin_rows(Some(message)),
            Ok(PluginPanelOutcome::RolledBack { id, restored }) => {
                self.refresh_plugin_rows(Some(format!("rolled `{id}` back to {restored}")));
            }
            Err(error) => {
                if let Some(panel) = self.plugin_panel.as_mut() {
                    panel.note_failure(error);
                }
            }
        }
    }

    /// Current MCP panel view.
    #[must_use]
    pub fn mcp_panel(&self) -> Option<&McpPanelView> {
        self.mcp_panel.as_ref()
    }

    /// Open the MCP panel over the current definitions and live registry.
    pub fn open_mcp_panel(&mut self) {
        if !self.claim_panel_surface() {
            return;
        }
        // Opening reads definitions. Connecting to every configured server
        // because a user pressed a key to LOOK at them would make the panel
        // the slowest thing in the product; "Refresh and probe" asks.
        match self.list_mcp_rows(McpListingDepth::Stored) {
            // An empty panel teaches nothing. The reference answers a zero-server
            // `/mcp` with a receipt that names the way to configure one.
            Ok(rows) if rows.is_empty() => self.items.push(Item::Info(
                "No MCP servers configured. Add one with `heycode mcp add`, or declare \
                 `[mcp.servers]` in your heycode configuration. Run `heycode doctor` if this is \
                 unexpected — it reports configuration files that failed validation."
                    .to_owned(),
            )),
            Ok(rows) => {
                self.mcp_panel = Some(McpPanelView::new(rows, McpListingSupport::CURRENT));
            }
            Err(error) => self.items.push(Item::Error(error)),
        }
    }

    /// Re-read the open panel and probe every server, as the panel's
    /// "Refresh and probe" action does.
    pub fn refresh_mcp_panel_probed(&mut self) {
        self.refresh_mcp_rows(Some("refreshed".to_owned()), McpListingDepth::Probed);
    }

    fn list_mcp_rows(&self, depth: McpListingDepth) -> Result<Vec<McpPanelRow>, String> {
        let Some(management) = self.mcp_management.as_ref() else {
            return Err("MCP management is not attached".to_owned());
        };
        let intent = match depth {
            McpListingDepth::Stored => McpPanelIntent::List,
            McpListingDepth::Probed => McpPanelIntent::ListProbed,
        };
        let status = match dispatch(management, &intent)? {
            McpPanelOutcome::Listed(status) => status,
            // `list` answers `Listed`; anything else is an operations-layer
            // contract break, reported rather than absorbed.
            other => return Err(format!("unexpected MCP list outcome: {other:?}")),
        };
        let snapshot = self
            .mcp_registry
            .as_ref()
            .and_then(|registry| registry.snapshot().ok());
        Ok(build_rows(&status, snapshot.as_deref()))
    }

    fn refresh_mcp_rows(&mut self, notice: Option<String>, depth: McpListingDepth) {
        let rows = self.list_mcp_rows(depth);
        let Some(panel) = self.mcp_panel.as_mut() else {
            return;
        };
        match rows {
            Ok(rows) => {
                panel.set_rows(rows);
                if let Some(notice) = notice {
                    panel.note_success(notice);
                }
            }
            Err(error) => panel.note_failure(error),
        }
    }

    fn handle_mcp_panel_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        let outcome = match self.mcp_panel.as_mut() {
            Some(panel) => panel.handle_key(code, modifiers),
            None => return,
        };
        match outcome {
            McpPanelKeyOutcome::Handled => {}
            McpPanelKeyOutcome::Close => self.close_mcp_panel(),
            McpPanelKeyOutcome::Run(intent) => self.run_mcp_intent(&intent),
        }
    }

    fn run_mcp_intent(&mut self, intent: &McpPanelIntent) {
        let Some(management) = self.mcp_management.clone() else {
            if let Some(panel) = self.mcp_panel.as_mut() {
                panel.note_failure("MCP management is not attached");
            }
            return;
        };
        match dispatch(&management, intent) {
            // Rows are republished only from a fresh read after the store
            // committed, never from the operation's own optimism.
            // A probing refresh must not be re-listed without probing, or the
            // health it just measured would be thrown away.
            Ok(McpPanelOutcome::Listed(_)) => self.refresh_mcp_rows(
                Some("refreshed".to_owned()),
                match intent {
                    McpPanelIntent::ListProbed => McpListingDepth::Probed,
                    _ => McpListingDepth::Stored,
                },
            ),
            Ok(McpPanelOutcome::Committed(message)) => {
                self.refresh_mcp_rows(Some(message), McpListingDepth::Stored);
            }
            Ok(McpPanelOutcome::Health { name, health }) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.note_success(format!(
                        "{name}: {} — {}",
                        health_word(health),
                        health_next_step(health)
                    ));
                }
                self.refresh_mcp_rows(None, McpListingDepth::Stored);
            }
            Err(error) => {
                if let Some(panel) = self.mcp_panel.as_mut() {
                    panel.note_failure(error);
                }
            }
        }
    }

    /// Bind the highest-priority U01 dialog to its exact live trust service.
    pub fn receive_workspace_trust(&mut self, prompt: heycode_trust::WorkspaceTrustPrompt) {
        self.close_capability_catalog();
        self.profile_picker = None;
        self.session_browser = None;
        self.theme_picker = None;
        self.advisor_panel = None;
        self.rewind_picker = None;
        self.keymap_picker = None;
        self.help_panel = None;
        self.memory_panel = None;
        self.skill_doctor_panel = None;
        self.copy_panel = None;
        self.scroll_speed_picker = None;
        let selected = prompt
            .state()
            .actions()
            .iter()
            .position(|action| *action == heycode_trust::WorkspaceTrustAction::OpenRestricted)
            .unwrap_or(0);
        self.workspace_trust = Some(WorkspaceTrustView {
            prompt,
            selected,
            error: None,
        });
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
    }

    /// Whether trust owns the foreground after account connection is complete.
    #[must_use]
    pub fn workspace_trust_is_foreground(&self) -> bool {
        self.workspace_trust.is_some() && !self.onboarding.as_ref().is_some_and(|view| view.active)
    }

    /// Current typed workspace-trust modal, when startup is blocked.
    #[must_use]
    pub fn workspace_trust(&self) -> Option<&WorkspaceTrustView> {
        self.workspace_trust.as_ref()
    }

    /// Take one typed terminal/recomposition outcome.
    pub fn take_run_outcome(&mut self) -> Option<TuiRunOutcome> {
        self.run_outcome.take()
    }

    /// Open the permission picker from one authoritative E10 report.
    pub fn open_permission_picker(&mut self, _report: heycode_exec::SandboxCapabilityReport) {
        self.close_capability_catalog();
        self.session_browser = None;
        self.close_settings_panel();
        self.profile_picker = None;
        self.session_browser = None;
        if self.workspace_trust.is_some()
            || self.pending_secret.is_some()
            || self.pending_ask.is_some()
            || self.pending_command_confirmation.is_some()
            || self
                .onboarding
                .as_ref()
                .is_some_and(|onboarding| onboarding.active)
        {
            return;
        }
        self.command_palette = None;
        self.sandbox_panel = None;
        self.autocompact_panel = None;
        self.pending_sandbox_selection = None;
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        let mut rows = build_permission_rows(&self.permission, self.can_cycle_approval_mode());
        if self.runtime != "native" {
            for row in &mut rows {
                if row.mode == heycode_agent::ApprovalPolicyKind::Plan {
                    row.selectable = false;
                    row.unavailable_reason = Some("Plan requires the native heycode runtime");
                }
            }
        }
        let selected = rows.iter().position(|row| row.current).unwrap_or(0);
        self.permission_picker = Some(PermissionPickerView { rows, selected });
    }

    /// Take one locally validated selection intent.
    ///
    /// This does not mutate or persist the effective sandbox service. The
    /// quiescent recomposition owner must reopen before any status may change.
    pub fn take_sandbox_selection(&mut self) -> Option<heycode_exec::SandboxMode> {
        self.pending_sandbox_selection.take()
    }

    /// Install an effective-state welcome snapshot and align compact status fields.
    pub fn set_welcome(&mut self, welcome: WelcomeStatusView) {
        self.set_active_model_id(welcome.model.clone());
        if self.runtime != welcome.runtime {
            self.clear_model_metadata();
        }
        self.runtime = welcome.runtime.clone();
        self.provider = welcome.provider.clone();
        self.permission = welcome.permission.clone();
        self.cwd = welcome.workspace.clone();
        self.welcome = Some(welcome);
    }

    /// Open the provider/runtime picker and request a live registry snapshot.
    pub fn open_route_picker(
        &mut self,
        current_provider: impl Into<String>,
        current_runtime: impl Into<String>,
    ) {
        self.close_capability_catalog();
        self.close_settings_panel();
        self.profile_picker = None;
        self.session_browser = None;
        if self.workspace_trust.is_some() {
            return;
        }
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        let current_provider = current_provider.into();
        let current_runtime = current_runtime.into();
        self.route_picker_request = Some((current_provider.clone(), current_runtime.clone()));
        self.route_picker = Some(RoutePickerView {
            current_provider,
            current_runtime,
            query: String::new(),
            filter: RoutePickerFilter::All,
            rows: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
        });
    }

    /// Apply a live registry projection to the open route picker.
    pub fn apply_route_catalog(&mut self, rows: Vec<RoutePickerRow>) {
        if let Some(picker) = self.route_picker.as_mut() {
            picker.rows = rows;
            picker.loading = false;
            picker.error = None;
        }
        self.refresh_route_matches();
    }

    /// Apply a safe route-registry projection failure.
    pub fn apply_route_catalog_error(&mut self, error: impl Into<String>) {
        if let Some(picker) = self.route_picker.as_mut() {
            picker.loading = false;
            picker.error = Some(error.into());
            picker.rows.clear();
            picker.matches.clear();
            picker.selected = 0;
        }
    }

    /// Take one request for a fresh combined registry snapshot.
    pub fn take_route_picker_request(&mut self) -> Option<(String, String)> {
        self.route_picker_request.take()
    }

    /// Take one activatable route selection.
    pub fn take_route_selection(&mut self) -> Option<RoutePickerSelection> {
        self.pending_route_selection.take()
    }

    /// Update health after the async doctor task settles.
    pub fn set_welcome_health(&mut self, health: WelcomeHealth) {
        if let Some(welcome) = self.welcome.as_mut() {
            welcome.health = health;
        }
    }

    /// Project one S11 report into the welcome card.
    pub fn apply_doctor_report(&mut self, report: &heycode_doctor::DoctorReport) {
        self.set_welcome_health(if report.healthy {
            WelcomeHealth::Healthy {
                passed: report.summary.passed,
                warnings: report.summary.warnings,
            }
        } else {
            WelcomeHealth::Unhealthy {
                failed: report.summary.failed,
                skipped: report.summary.skipped,
            }
        });
    }

    /// Attach optional user/project catalog assertions for visible attribution.
    pub fn set_catalog_overrides(
        &mut self,
        overrides: Option<Arc<heycode_catalog_file::CatalogOverrides>>,
    ) {
        self.catalog_overrides = overrides;
    }

    /// Open a non-blocking picker and request a cache-aware refresh.
    pub fn open_model_picker(
        &mut self,
        owner: BackendControlOwner,
        routing_revision: u64,
        current_model: impl Into<String>,
    ) {
        self.close_capability_catalog();
        self.close_settings_panel();
        self.profile_picker = None;
        if self.workspace_trust.is_some() {
            return;
        }
        self.command_palette = None;
        self.close_permission_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.model_picker = Some(ModelPickerView {
            owner,
            routing_revision,
            current_model: current_model.into(),
            query: String::new(),
            search_active: false,
            filter: ModelPickerFilter::Selectable,
            matches: Vec::new(),
            selected: 0,
            unmatched_overrides: Vec::new(),
            state: ModelPickerLoadState::Loading,
            effort_model: None,
            effort_picker: None,
            effort_error: None,
        });
        self.model_refresh_request = Some(heycode_llm::CatalogRefreshMode::PreferCache);
    }

    /// Apply one successful catalog read and preserve current query/filter.
    pub fn apply_model_catalog(&mut self, view: heycode_llm::CatalogView) {
        let Some(owner) = self
            .model_picker
            .as_ref()
            .map(|picker| picker.owner.clone())
        else {
            return;
        };
        self.apply_model_catalog_for(&owner, view);
    }

    fn apply_model_catalog_for(
        &mut self,
        owner: &BackendControlOwner,
        view: heycode_llm::CatalogView,
    ) {
        let Some(picker) = self.model_picker.as_mut() else {
            return;
        };
        if &picker.owner != owner {
            return;
        }
        if view.snapshot.provider.id != picker.owner.id() {
            picker.state = ModelPickerLoadState::Error {
                message: "model catalog provider changed during refresh".to_owned(),
            };
            picker.matches.clear();
            picker.unmatched_overrides.clear();
            return;
        }
        picker.state = ModelPickerLoadState::Ready {
            snapshot: view.snapshot,
            freshness: view.freshness,
            warning: view.warning.map(|warning| match warning {
                heycode_llm::CatalogError::Refresh { kind, message, .. } => {
                    format!("{kind:?}: {message}")
                }
                other => other.to_string(),
            }),
        };
        self.refresh_model_matches();
    }

    /// Apply a safe catalog error when no generation is available.
    pub fn apply_model_catalog_error(&mut self, error: impl Into<String>) {
        let Some(owner) = self
            .model_picker
            .as_ref()
            .map(|picker| picker.owner.clone())
        else {
            return;
        };
        self.apply_model_catalog_error_for(&owner, error);
    }

    fn apply_model_catalog_error_for(
        &mut self,
        owner: &BackendControlOwner,
        error: impl Into<String>,
    ) {
        if let Some(picker) = self
            .model_picker
            .as_mut()
            .filter(|picker| &picker.owner == owner)
        {
            picker.state = ModelPickerLoadState::Error {
                message: error.into(),
            };
            picker.matches.clear();
            picker.selected = 0;
            picker.unmatched_overrides.clear();
        }
    }

    /// Take one cache-aware/forced refresh request for the event loop.
    pub fn take_model_refresh_request(
        &mut self,
    ) -> Option<(BackendControlOwner, heycode_llm::CatalogRefreshMode)> {
        let mode = self.model_refresh_request.take()?;
        let owner = self.model_picker.as_ref()?.owner.clone();
        Some((owner, mode))
    }

    /// Take the request to cancel an in-flight picker wait.
    pub fn take_model_refresh_cancel(&mut self) -> bool {
        std::mem::take(&mut self.model_refresh_cancel_requested)
    }

    /// Take one catalog-proven model with its captured backend/revision token.
    pub fn take_model_selection(&mut self) -> Option<ModelPickerSelection> {
        self.pending_model_selection.take()
    }

    fn take_model_effort_request(&mut self) -> Option<(BackendControlOwner, u64, String)> {
        let picker = self.model_picker.as_mut()?;
        let model = &picker.matches.get(picker.selected)?.model.id;
        if picker.effort_model.as_ref() == Some(model) {
            return None;
        }
        picker.effort_model = Some(model.clone());
        picker.effort_picker = None;
        picker.effort_error = None;
        Some((picker.owner.clone(), picker.routing_revision, model.clone()))
    }

    fn apply_model_effort_catalog(
        &mut self,
        owner: &BackendControlOwner,
        revision: u64,
        model: &str,
        result: Result<heycode_routing::BackendEffortCatalog, String>,
    ) {
        let Some(picker) = self.model_picker.as_mut().filter(|picker| {
            picker.owner == *owner
                && picker.routing_revision == revision
                && picker
                    .matches
                    .get(picker.selected)
                    .is_some_and(|row| row.model.id == model)
                && picker.effort_model.as_deref() == Some(model)
        }) else {
            return;
        };
        match result {
            Ok(catalog) => {
                picker.effort_picker = Some(EffortPickerView::new(
                    owner.clone(),
                    revision,
                    catalog.current().map(str::to_owned),
                    catalog.choices().to_vec(),
                    catalog.default().map(str::to_owned),
                ));
                picker.effort_error = None;
            }
            Err(error) => {
                picker.effort_error = Some(error);
            }
        }
    }

    /// Open a picker over one backend's already-validated effort metadata.
    pub fn open_effort_picker(
        &mut self,
        owner: BackendControlOwner,
        routing_revision: u64,
        current_effort: Option<String>,
        choices: Vec<String>,
        default_effort: Option<String>,
    ) {
        self.close_capability_catalog();
        self.close_settings_panel();
        self.profile_picker = None;
        self.session_browser = None;
        if self.high_priority_modal_open() {
            return;
        }
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.effort_picker = Some(EffortPickerView::new(
            owner,
            routing_revision,
            current_effort,
            choices,
            default_effort,
        ));
    }

    /// Take one exact effort with the backend/revision token captured at open.
    pub fn take_effort_selection(
        &mut self,
    ) -> Option<(
        BackendControlOwner,
        u64,
        String,
        heycode_routing::SelectionScope,
    )> {
        self.pending_effort_selection.take()
    }

    /// Number of commands waiting for active work to settle.
    #[must_use]
    pub fn queued_command_count(&self) -> usize {
        self.queued_commands.len()
    }

    /// Promote one exact queued command after the runner has proved all active
    /// turn and command tasks settled.
    ///
    /// The guard is the same predicate the dispatcher uses to re-queue: while
    /// the turn is still marked active nothing is promoted, so a settled task
    /// racing a late `TurnStarted` cannot make the two spin.
    pub fn promote_next_queued_command(&mut self) {
        if !self.connection_setup_is_active()
            && !self.active_turn
            && self.pending_send.is_none()
            && let Some(command) = self.queued_commands.pop_front()
        {
            self.items
                .push(Item::Info(format!("running {}", command.synopsis)));
            self.pending_send = Some(command.text);
        }
    }

    /// What the status line should say the agent is doing right now.
    ///
    /// Context meter facts: the reading plus whether it is an estimate.
    /// Percent is `None` when no window is configured.
    #[must_use]
    pub fn context_meter(&self) -> Option<ContextMeter> {
        let tokens = self.context_tokens?;
        let estimated = self.context_tokens_estimated;
        let Some(context_window) = self.context_window.filter(|window| *window > 0) else {
            return Some(ContextMeter {
                tokens,
                percent: None,
                warn: false,
                estimated,
            });
        };
        let percent = tokens.saturating_mul(100) / context_window;
        #[allow(clippy::cast_precision_loss)]
        let ratio = tokens as f64 / context_window as f64;
        Some(ContextMeter {
            tokens,
            percent: Some(percent),
            warn: self.context_budget.as_ref().map_or(
                ratio >= f64::from(self.context_warn_ratio),
                |budget| {
                    budget.auto_compact
                        && budget
                            .compact_at
                            .is_some_and(|threshold| tokens >= threshold.saturating_mul(9) / 10)
                },
            ),
            estimated,
        })
    }

    /// While a permission card is open the agent is waiting on the user, not
    /// working, and a busy verb ("Forging…") there reads as a hang.
    #[must_use]
    pub fn activity_label(&self) -> Option<&str> {
        if self.pending_ask.is_some() {
            return Some("waiting for approval");
        }
        self.side_command_activity.or(self.verb.as_deref())
    }

    /// Whether an agent turn is scheduled or running. This is lifecycle state,
    /// unlike the optional spinner verb used only for presentation.
    #[must_use]
    pub const fn has_active_turn(&self) -> bool {
        self.active_turn
    }

    /// Whether composer steering/follow-up may use the native Agent inbox.
    ///
    /// Delegated runtimes own different control protocols; falling back to the
    /// composed native Agent would send input to the wrong session.
    #[must_use]
    pub fn native_inbox_available(&self) -> bool {
        self.runtime == "native"
    }

    /// Pending durable inbox counts last published by the Agent.
    #[must_use]
    pub const fn inbox_pending(&self) -> heycode_agent::InboxPending {
        self.inbox_pending
    }

    /// Take one composer submission intended for the durable operational
    /// inbox.
    pub fn take_inbox_submission(&mut self) -> Option<(heycode_session::InboxDelivery, String)> {
        self.pending_inbox_submission.pop_front()
    }

    /// Consume one settlement-time follow-up wake.
    ///
    /// One wake has one owner. A later settlement publishes another wake when
    /// more next-turn input remains.
    pub fn take_follow_up_wake(&mut self) -> bool {
        if self.connection_setup_is_active() {
            return false;
        }
        std::mem::take(&mut self.follow_up_wake_pending) && !self.inbox_pending.is_empty()
    }

    fn connection_setup_is_active(&self) -> bool {
        self.onboarding.as_ref().is_some_and(|view| view.active)
    }

    /// Whether mandatory first-run setup owns the whole viewport.
    ///
    /// First run has nothing to show behind the wizard, so it keeps the
    /// centred card. `/login` and `/connect` reopen the same wizard inside a
    /// running session, where the reference draws a bottom panel over a live
    /// transcript instead — see [`Self::onboarding_is_panel`].
    #[must_use]
    pub fn onboarding_is_fullscreen(&self) -> bool {
        self.onboarding
            .as_ref()
            .is_some_and(|view| view.active && !view.from_connect)
    }

    /// Whether the in-session `/login` panel is open at the bottom of the shell.
    #[must_use]
    pub fn onboarding_is_panel(&self) -> bool {
        self.onboarding
            .as_ref()
            .is_some_and(|view| view.active && view.from_connect)
    }

    fn mark_turn_scheduled(&mut self) {
        if !self.active_turn {
            self.activity_item_start = self.items.len();
            self.cancellation_requested = false;
        }
        self.turn_started_at
            .get_or_insert_with(std::time::Instant::now);
        self.active_turn = true;
        self.verb.get_or_insert_with(|| "Thinking…".to_owned());
    }

    pub(crate) fn is_compacting(&self) -> bool {
        self.active_command_draft.is_some() && self.side_command_activity.is_none()
    }

    pub(crate) fn side_command_activity(&self) -> Option<&'static str> {
        self.side_command_activity
    }

    fn begin_side_command(&mut self, text: &str) {
        self.capture_command_draft(text);
        self.side_command_activity = Some("Recapping conversation… (Esc to cancel)");
    }

    fn begin_compact_command(&mut self, text: &str) {
        self.capture_command_draft(text);
        self.verb = Some("Compacting conversation… (Esc to cancel)".to_owned());
    }

    fn capture_command_draft(&mut self, text: &str) {
        let draft = self
            .submitted_command_draft
            .take()
            .filter(|draft| draft.lines().join("\n") == text)
            .unwrap_or_else(|| {
                let mut draft = tui_textarea::TextArea::from(text.lines());
                draft.move_cursor(tui_textarea::CursorMove::Bottom);
                draft.move_cursor(tui_textarea::CursorMove::End);
                draft
            });
        self.active_command_draft = Some(draft);
    }

    fn restore_interrupted_command_draft(&mut self) {
        if let Some(draft) = self.active_command_draft.take() {
            if self.input.lines().iter().all(String::is_empty) {
                self.input = draft;
                self.history_cursor = None;
            } else {
                self.items.push(Item::Info(
                    "Command cancelled; your current draft was kept".to_owned(),
                ));
            }
        }
    }

    /// The relay sends its final events before its task returns. A ready join
    /// can win select ahead of those queued events, including TurnStarted on a
    /// failed request with no TurnFinished. Apply them before final settlement
    /// so an old start cannot resurrect Working after the owner has returned.
    fn settle_joined_turn(
        &mut self,
        events: &mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
        native_turn_active: impl FnOnce() -> bool,
    ) {
        let queued = events.len();
        for _ in 0..queued {
            let Ok(event) = events.try_recv() else { break };
            if !session_owned_ui_event(&event) {
                self.apply(&event);
            }
        }
        // An old foreground relay cannot settle an independently owned inbox
        // turn. Sample after draining queued events so a queued newer finish
        // cannot be followed by a stale synthetic Working state.
        if !native_turn_active() {
            self.mark_turn_settled();
        } else if !self.has_active_turn() {
            self.mark_turn_scheduled();
        }
    }

    fn mark_turn_settled(&mut self) {
        self.cancellation_requested = false;
        self.turn_started_at = None;
        self.active_turn = false;
        self.verb = None;
        self.spinner = 0;
        // Runtime questions and permissions belong to the foreground turn.
        // Local approvals can also belong to background work; retain those
        // only when their actual waiter is still live.
        if let Some(ask) = self.pending_ask.take() {
            self.queued_asks.push_front(ask);
        }
        self.queued_asks.retain(|ask| {
            !self.runtime_permission_ids.contains_key(&ask.id)
                && self
                    .approvals
                    .as_ref()
                    .is_some_and(|policy| policy.is_pending(ask.id))
        });
        self.pending_ask = self.queued_asks.pop_front();
        self.runtime_permission_ids.clear();
        self.pending_runtime_permission_response = None;
        self.pending_runtime_question = None;
    }

    fn queue_inbox_submission(&mut self, delivery: heycode_session::InboxDelivery, text: String) {
        self.pending_inbox_submission.push_back((delivery, text));
        self.input = tui_textarea::TextArea::default();
    }

    fn queue_follow_up(&mut self) {
        if !self.active_turn {
            return;
        }
        let text = self.input.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        if !self.native_inbox_available() {
            self.items.push(Item::Info(
                "follow-up is unavailable for this delegated runtime in the current TUI".to_owned(),
            ));
            return;
        }
        if parse_slash(&text).is_some() {
            self.items.push(Item::Info(
                "slash commands use Enter so their scheduling policy remains explicit".to_owned(),
            ));
            return;
        }
        self.queue_inbox_submission(heycode_session::InboxDelivery::FollowUp, text);
    }

    fn queue_command(&mut self, text: String, synopsis: String) {
        self.items.push(Item::Info(format!(
            "queued {synopsis} — runs after active work"
        )));
        self.queued_commands
            .push_back(QueuedCommand { text, synopsis });
    }

    fn request_command_confirmation(&mut self, text: String, synopsis: String) {
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.pending_command_confirmation = Some(PendingCommandConfirmation {
            text,
            synopsis,
            selection: 0,
        });
    }

    fn set_active_model_id(&mut self, model: String) {
        if self.model != model {
            self.clear_model_metadata();
            self.context_tokens = None;
        }
        self.model = model;
    }

    fn clear_model_metadata(&mut self) {
        self.model_display_name = None;
        self.resolved_model = None;
        self.model_description = None;
        self.context_window = None;
        self.context_budget = None;
        self.model_default_reasoning_effort = None;
        self.recompute_reasoning_effort();
    }

    fn set_configured_reasoning_effort(&mut self, effort: Option<String>) {
        self.configured_reasoning_effort = effort;
        self.recompute_reasoning_effort();
    }

    fn recompute_reasoning_effort(&mut self) {
        if let Some(effort) = self.configured_reasoning_effort.as_ref() {
            self.reasoning_effort = Some(effort.clone());
            self.reasoning_effort_is_default = false;
        } else {
            self.reasoning_effort
                .clone_from(&self.model_default_reasoning_effort);
            self.reasoning_effort_is_default = self.reasoning_effort.is_some();
        }
    }

    /// Apply metadata supplied by the exact live runtime session. The model
    /// wire value must still match so a late response cannot relabel a newer
    /// selection.
    pub fn apply_runtime_model_configuration(
        &mut self,
        configuration: &heycode_app_server::AppRuntimeModelConfiguration,
    ) {
        self.apply_model_configuration_fields(ModelConfigurationFields {
            model: &configuration.model,
            display_name: &configuration.display_name,
            resolved_model: configuration.resolved_model.as_deref(),
            description: configuration.description.as_deref(),
            context_window: configuration.context_window,
            default_reasoning_effort: configuration.default_reasoning_effort.as_deref(),
            reasoning_efforts: &configuration.reasoning_efforts,
        });
    }

    fn apply_discovered_runtime_model_configuration(
        &mut self,
        configuration: &heycode_runtime::RuntimeModelConfiguration,
    ) {
        self.apply_model_configuration_fields(ModelConfigurationFields {
            model: &configuration.model,
            display_name: &configuration.display_name,
            resolved_model: configuration.resolved_model.as_deref(),
            description: configuration.description.as_deref(),
            context_window: configuration.context_window,
            default_reasoning_effort: configuration.default_reasoning_effort.as_deref(),
            reasoning_efforts: &configuration.reasoning_efforts,
        });
    }

    fn apply_model_configuration_fields(&mut self, fields: ModelConfigurationFields<'_>) {
        if fields.model != self.model {
            return;
        }
        self.model_display_name =
            (!fields.display_name.is_empty()).then(|| fields.display_name.to_owned());
        if let Some(resolved_model) = fields.resolved_model {
            self.resolved_model = Some(resolved_model.to_owned());
        }
        self.model_description = fields.description.map(str::to_owned);
        if self.context_budget.is_none()
            && let Some(window) = fields.context_window.filter(|window| *window > 0)
        {
            self.context_window = Some(window);
        }
        self.model_default_reasoning_effort = fields
            .default_reasoning_effort
            .filter(|default| {
                fields
                    .reasoning_efforts
                    .iter()
                    .any(|choice| choice == default)
            })
            .map(str::to_owned);
        self.recompute_reasoning_effort();
    }

    /// Apply the normalized descriptor from the exact catalog generation
    /// used to validate a model selection.
    pub fn apply_model_descriptor(&mut self, model: &heycode_llm::ModelDescriptor) {
        if model.id != self.model {
            return;
        }
        self.model_display_name = Some(model.display_name.clone());
        if self.context_budget.is_none() {
            self.context_window = model.context_window.filter(|window| *window > 0);
        }
    }

    /// Human label for the active model while `model` remains the exact wire
    /// value used by pickers and configuration updates.
    #[must_use]
    pub fn active_model_label(&self) -> &str {
        self.resolved_model
            .as_deref()
            .or(self.model_display_name.as_deref())
            .unwrap_or(&self.model)
    }

    fn apply_active_routing_configuration(
        &mut self,
        active: &heycode_routing::ActiveRoutingConfiguration,
    ) {
        match active.owner() {
            BackendControlOwner::NativeInference { provider } => {
                self.provider.clone_from(provider);
                if let Some(welcome) = self.welcome.as_mut() {
                    welcome.provider.clone_from(provider);
                }
            }
            BackendControlOwner::DelegatedRuntime { runtime } => {
                self.update_welcome_runtime(runtime.clone());
                self.provider.clone_from(runtime);
                if let Some(welcome) = self.welcome.as_mut() {
                    welcome.provider.clone_from(runtime);
                }
            }
        }
        self.set_active_model_id(active.model().unwrap_or_default().to_owned());
        self.set_configured_reasoning_effort(active.effort().map(str::to_owned));
        if let Some(welcome) = self.welcome.as_mut() {
            welcome.model.clone_from(&self.model);
        }
    }

    /// Reflect a live approval-mode change in the status line and welcome card.
    fn update_welcome_permission(&mut self, permission: String) {
        self.permission = permission.clone();
        if let Some(welcome) = self.welcome.as_mut() {
            welcome.permission = permission;
        }
    }

    fn update_welcome_runtime(&mut self, runtime: String) {
        if self.runtime != runtime {
            self.clear_model_metadata();
            self.context_tokens = None;
        }
        self.runtime = runtime.clone();
        if let Some(welcome) = self.welcome.as_mut() {
            welcome.runtime = runtime;
        }
    }

    /// Attach the plugin-owned onboarding state machine.
    pub fn set_onboarding(&mut self, service: Arc<heycode_onboarding::OnboardingService>) {
        self.close_capability_catalog();
        self.profile_picker = None;
        self.session_browser = None;
        self.theme_picker = None;
        self.advisor_panel = None;
        self.rewind_picker = None;
        self.keymap_picker = None;
        self.help_panel = None;
        self.memory_panel = None;
        self.skill_doctor_panel = None;
        self.copy_panel = None;
        self.scroll_speed_picker = None;
        match service.snapshot() {
            Ok(snapshot) => {
                if snapshot.active {
                    self.close_permission_picker();
                }
                self.onboarding = Some(snapshot);
                self.onboarding_service = Some(service);
            }
            Err(error) => self.items.push(Item::Error(error.to_string())),
        }
    }

    /// Attach the interactive masked secret broker.
    pub fn set_secret_prompt(
        &mut self,
        prompt: Arc<heycode_authorization_api_key::InteractiveSecretPrompt>,
    ) {
        self.secret_prompt = Some(prompt);
    }

    /// Apply a safe broker notification (never contains secret text).
    pub fn apply_secret_prompt(
        &mut self,
        event: &heycode_authorization_api_key::SecretPromptNotification,
    ) {
        use heycode_authorization_api_key::SecretPromptNotification;
        match event {
            SecretPromptNotification::Requested {
                id,
                prompt,
                error,
                query,
                masked,
                ..
            } => {
                self.close_capability_catalog();
                self.profile_picker = None;
                self.session_browser = None;
                self.theme_picker = None;
                self.advisor_panel = None;
                self.rewind_picker = None;
                self.keymap_picker = None;
                self.help_panel = None;
                self.memory_panel = None;
                self.skill_doctor_panel = None;
                self.copy_panel = None;
                self.scroll_speed_picker = None;
                self.command_palette = None;
                self.close_permission_picker();
                self.close_model_picker();
                self.close_effort_picker();
                self.close_route_picker();
                self.close_mcp_panel();
                self.close_plugin_panel();
                self.pending_secret = Some(PendingSecretView {
                    id: *id,
                    prompt: prompt.clone(),
                    error: error.clone(),
                    reference: query.reference.as_str().to_owned(),
                    masked: *masked,
                    secret: String::new(),
                });
            }
            SecretPromptNotification::Resolved { id, .. } => {
                if self
                    .pending_secret
                    .as_ref()
                    .is_some_and(|view| view.id == *id)
                {
                    self.pending_secret = None;
                }
            }
        }
    }

    /// Apply one live event, mutating transcript/state.
    pub fn apply(&mut self, event: &UiEvent) {
        match event {
            UiEvent::UserEcho { text } => {
                if self.chrome.animation() {
                    self.companion.message(text);
                }
                self.items.push(Item::User(text.clone()));
            }
            UiEvent::UserAttachmentsEcho {
                attachments,
                document_routes,
            } => {
                self.items.push(Item::Attachments {
                    attachments: attachments.clone(),
                    document_routes: document_routes.clone(),
                });
                self.pending_attachments.clear();
            }
            UiEvent::AttachmentComposerRequested { action } => match action {
                heycode_agent::AttachmentComposerAction::Add(attachment) => {
                    if self.pending_attachments.len() >= 16 {
                        self.items.push(Item::Error(
                            "at most sixteen attachments may accompany one message".to_owned(),
                        ));
                    } else if self
                        .pending_attachments
                        .iter()
                        .any(|candidate| candidate.content_id() == attachment.content_id())
                    {
                        self.items
                            .push(Item::Info("attachment is already staged".to_owned()));
                    } else {
                        self.pending_attachments.push(attachment.clone());
                        self.items.push(Item::Info(format!(
                            "staged {} for the next message",
                            attachment.display_name().unwrap_or("attachment")
                        )));
                    }
                }
                heycode_agent::AttachmentComposerAction::Clear => {
                    self.pending_attachments.clear();
                    self.items
                        .push(Item::Info("cleared pending attachments".to_owned()));
                }
            },
            UiEvent::AssistantDelta { text } => {
                self.finish_reasoning(false);
                append_stream(
                    &mut self.items,
                    |i| matches!(i, Item::Assistant(_)),
                    || Item::Assistant(String::new()),
                    |item| {
                        if let Item::Assistant(buf) = item {
                            buf.push_str(text);
                        }
                    },
                );
            }
            UiEvent::AssistantAudio { attachments } => {
                self.items.push(Item::AudioOutput {
                    attachments: attachments.clone(),
                });
            }
            UiEvent::ReasoningDelta { text } => {
                // Providers may report opaque reasoning activity with an empty
                // delta. It is not readable content and must not create a
                // disclosure control. Preserve whitespace within visible text.
                if text.trim().is_empty()
                    && !matches!(self.items.last(), Some(Item::Reasoning { done: false, .. }))
                {
                    return;
                }
                append_stream(
                    &mut self.items,
                    |i| matches!(i, Item::Reasoning { done: false, .. }),
                    || Item::Reasoning {
                        text: String::new(),
                        done: false,
                        view: ReasoningView::live(),
                    },
                    |item| {
                        if let Item::Reasoning { text: buf, .. } = item {
                            buf.push_str(text);
                        }
                    },
                );
            }
            UiEvent::ToolStarted { name, args } => {
                self.finish_reasoning(false);
                self.items.push(Item::Tool {
                    call_id: None,
                    name: name.clone(),
                    args: args.clone(),
                    result: None,
                    untrusted_content: None,
                    view: ToolViewState::default(),
                });
                self.bind_pending_tool_approval(self.items.len() - 1);
            }
            UiEvent::ToolFinished {
                ok,
                value,
                untrusted_content,
                ..
            } => {
                self.tool_group_signature = None;
                if let Some(index) = self
                    .items
                    .iter()
                    .rposition(|item| matches!(item, Item::Tool { result: None, .. }))
                {
                    if let Item::Tool {
                        result,
                        untrusted_content: boundary,
                        ..
                    } = &mut self.items[index]
                    {
                        *result = Some((*ok, value.clone()));
                        *boundary = *untrusted_content;
                    }
                    self.merge_job_output(index);
                }
            }
            UiEvent::Status { verb } => {
                if !self.is_compacting() {
                    self.verb = Some(verb.clone());
                }
            }
            UiEvent::TurnStarted { .. } | UiEvent::RuntimeTurnStarted { .. } => {
                self.turn_error = TurnErrorRow::None;
                self.mark_turn_scheduled();
            }
            // Collapse any open reasoning block when the turn settles.
            UiEvent::TurnFinished {
                usage,
                reason,
                context_tokens,
            } => {
                self.finish_reasoning(reason == "aborted" || reason == "error");
                self.usage = *usage;
                // Only an explicit current-context measurement belongs in the
                // context meter. Delegated runtimes can report cumulative
                // prompt usage across several model calls; that remains usage
                // evidence and must not masquerade as the live context size.
                if let Some(estimate) = context_tokens
                    && self.context_budget.is_none()
                {
                    self.context_tokens = Some(*estimate);
                    self.context_tokens_estimated = true;
                }
                self.mark_turn_settled();
                if self.chrome.animation() {
                    self.companion.react(if reason == "error" {
                        crate::mascot::Reaction::Concern
                    } else if reason == "aborted" {
                        crate::mascot::Reaction::Listen
                    } else {
                        crate::mascot::Reaction::Purr
                    });
                }
                if reason == "error" && self.turn_error == TurnErrorRow::None {
                    self.items
                        .push(Item::Error("turn ended with an error".to_owned()));
                    self.turn_error = TurnErrorRow::Generic(self.items.len() - 1);
                }
            }
            UiEvent::ContextBudgetChanged { budget } => {
                self.context_tokens = Some(budget.used);
                self.context_tokens_estimated =
                    budget.confidence != heycode_llm::ContextConfidence::Exact;
                self.context_window = budget.window;
                self.context_budget = Some(budget.clone());
            }
            UiEvent::RuntimeContextMeasured {
                resolved_model,
                tokens,
                context_window,
            } => {
                if *context_window > 0 {
                    self.context_budget = None;
                    if let Some(model) = resolved_model {
                        self.resolved_model = Some(model.clone());
                    }
                    self.context_tokens = Some(*tokens);
                    self.context_tokens_estimated = false;
                    self.context_window = Some(*context_window);
                }
            }
            UiEvent::Error { message } => {
                self.quiet_permission_mode = None;
                self.pending_edit_approval_id = None;
                let generic = matches!(
                    message.as_str(),
                    "provider error"
                        | "turn ended with an error"
                        | "runtime operation failed"
                        | "app-server operation failed: runtime operation failed"
                );
                if self.turn_error == TurnErrorRow::Specific
                    && (generic
                        || self.items.iter().rev().find_map(|item| match item {
                            Item::Error(text) => Some(text == message),
                            _ => None,
                        }) == Some(true))
                {
                    return;
                }
                let message = if generic {
                    "turn ended with an error"
                } else {
                    message.as_str()
                };
                let index = match self.turn_error {
                    TurnErrorRow::Generic(index) if matches!(self.items.get(index), Some(Item::Error(text)) if text == "turn ended with an error") =>
                    {
                        self.items[index] = Item::Error(message.to_owned());
                        index
                    }
                    _ => {
                        self.items.push(Item::Error(message.to_owned()));
                        self.items.len() - 1
                    }
                };
                self.turn_error = if generic {
                    TurnErrorRow::Generic(index)
                } else {
                    TurnErrorRow::Specific
                };
                self.scroll_from_bottom = 0;
            }
            UiEvent::Info { text } => {
                let shortcut_acknowledgment =
                    self.quiet_permission_mode.as_ref().is_some_and(|mode| {
                        text.starts_with(&format!(
                            "Permissions: {}.",
                            crate::permission_picker::permission_label(mode)
                        ))
                    });
                if shortcut_acknowledgment {
                    self.quiet_permission_mode = None;
                } else {
                    self.items.push(Item::Info(text.clone()));
                }
            }
            UiEvent::FindingsReported { report } => {
                self.items.push(Item::FindingsReport {
                    report: Box::new(report.clone()),
                    expanded: false,
                    focused: false,
                });
                self.scroll_from_bottom = 0;
            }
            UiEvent::HelpRequested { header, commands } => {
                if !self.high_priority_modal_open()
                    && self.pending_plan_review.is_none()
                    && self.pending_mcp_elicitation.is_none()
                {
                    self.close_low_priority_surfaces();
                    let header = if self.runtime.is_empty() || self.runtime == "native" {
                        header.clone()
                    } else {
                        format!("runtime: {}\nmodel: {}", self.runtime, self.model)
                    };
                    let mut custom_rows = Vec::new();
                    if let Some(packages) = self.plugin_packages.as_ref() {
                        for command in commands {
                            let source = command.descriptor.source().plugin();
                            if !packages.versions(source).is_empty() {
                                let description = match command.availability.reason() {
                                    Some(reason) => format!(
                                        "{} · unavailable: {reason}",
                                        command.descriptor.description()
                                    ),
                                    None => command.descriptor.description().to_owned(),
                                };
                                if let Some(row) = crate::help::CustomHelpRow::new(
                                    command.descriptor.synopsis(),
                                    description,
                                    format!("plugin: {source}"),
                                ) {
                                    custom_rows.push(row);
                                }
                            }
                        }
                    }
                    let mut custom_notice = None;
                    if let Some(skills) = self.skills.as_ref() {
                        match skills.snapshot_records() {
                            Ok(records) => {
                                for record in records {
                                    if let Some(row) = crate::help::CustomHelpRow::new(
                                        format!("/skill {}", record.skill.name),
                                        record.skill.description,
                                        format!("{} skill", record.source.scope().as_str()),
                                    ) {
                                        custom_rows.push(row);
                                    }
                                }
                            }
                            Err(_) => {
                                custom_notice = Some(
                                    "Custom skill catalog is unavailable in this session."
                                        .to_owned(),
                                )
                            }
                        }
                    }
                    self.help_panel = Some(
                        crate::help::HelpView::new(header, commands.clone(), &self.keymap)
                            .with_custom_rows(custom_rows)
                            .with_custom_notice(custom_notice),
                    );
                } else {
                    self.items.push(Item::Info(
                        "Close the current dialog before opening help.".to_owned(),
                    ));
                }
            }
            UiEvent::CapabilityPanelRequested { panel } => {
                if panel.as_str() == "skill-doctor" {
                    if self.high_priority_modal_open()
                        || self.pending_plan_review.is_some()
                        || self.pending_mcp_elicitation.is_some()
                    {
                        self.items.push(Item::Info(
                            "Close the current dialog before opening Skill Stats.".to_owned(),
                        ));
                    } else {
                        let result = self
                            .skills
                            .as_ref()
                            .zip(self.current_session.as_ref())
                            .ok_or_else(|| {
                                "Skill Stats are unavailable in this session.".to_owned()
                            })
                            .and_then(|(skills, session)| {
                                let session =
                                    session.lock().unwrap_or_else(|error| error.into_inner());
                                crate::skill_doctor_panel::SkillDoctorPanel::capture(
                                    skills, &session,
                                )
                                .map_err(|error| error.to_string())
                            });
                        match result {
                            Ok(panel) => {
                                self.close_low_priority_surfaces();
                                self.skill_doctor_panel = Some(panel);
                            }
                            Err(error) => self.items.push(Item::Error(error)),
                        }
                    }
                } else if panel.as_str() == crate::memory_commands::MEMORY_PANEL_ID {
                    if self.high_priority_modal_open()
                        || self.pending_plan_review.is_some()
                        || self.pending_mcp_elicitation.is_some()
                    {
                        self.items.push(Item::Info(
                            "Close the current dialog before opening memory.".to_owned(),
                        ));
                    } else if let Some(manager) = self.memory_sources.clone() {
                        self.close_low_priority_surfaces();
                        match crate::memory_panel::MemoryPanelView::new(manager) {
                            Ok(panel) => self.memory_panel = Some(panel),
                            Err(error) => self.items.push(Item::Error(error.to_string())),
                        }
                    } else {
                        self.items.push(Item::Error(
                            "Memory sources are unavailable in this session.".to_owned(),
                        ));
                    }
                } else if let Some(panel) = CapabilityPanel::from_id(panel) {
                    self.open_capability_panel(panel);
                } else {
                    self.items
                        .push(Item::Error("requested panel is unavailable".to_owned()));
                }
            }
            UiEvent::SettingsShellRequested { tab, snapshot } => {
                self.open_settings_shell(*tab, snapshot.clone());
            }
            UiEvent::ProfilePickerRequested => self.open_profile_picker(),
            UiEvent::ProfileSelected { name } => self.select_profile(Some(name.clone())),
            UiEvent::ModelPickerRequested {
                owner,
                routing_revision,
                current_model,
            } => self.open_model_picker(owner.clone(), *routing_revision, current_model.clone()),
            UiEvent::EffortPickerRequested {
                owner,
                routing_revision,
                current_effort,
                choices,
                default_effort,
            } => self.open_effort_picker(
                owner.clone(),
                *routing_revision,
                current_effort.clone(),
                choices.clone(),
                default_effort.clone(),
            ),
            UiEvent::RoutePickerRequested {
                current_provider,
                current_runtime,
            } => self.open_route_picker(current_provider.clone(), current_runtime.clone()),
            UiEvent::SandboxPanelRequested { report } => {
                if self.claim_panel_surface() {
                    self.sandbox_panel =
                        Some(crate::sandbox_panel::SandboxPanel::new(report.clone()));
                }
            }
            UiEvent::AutoCompactPickerRequested { enabled, .. } => {
                if self.claim_panel_surface() {
                    let result = self
                        .settings_service
                        .clone()
                        .ok_or_else(|| "Auto-compaction settings are unavailable.".to_owned())
                        .and_then(|settings| {
                            crate::autocompact_panel::AutoCompactPanel::open(settings, *enabled)
                        });
                    match result {
                        Ok(panel) => self.autocompact_panel = Some(panel),
                        Err(error) => self.items.push(Item::Error(error)),
                    }
                }
            }
            UiEvent::PermissionPickerRequested { report } => {
                self.open_permission_picker(report.clone());
            }
            UiEvent::LoggedOut {
                target,
                cleanup_warning,
            } => {
                // The reference prints one receipt and leaves. Dropping a
                // logged-out session straight into the setup chooser answers a
                // request to disconnect with a demand to reconnect.
                self.items
                    .push(Item::Info(format!("Disconnected {target}")));
                if let Some(warning) = cleanup_warning {
                    self.items.push(Item::Error(warning.clone()));
                }
                self.quit_requested = true;
            }
            UiEvent::ConnectRequested => {
                if let Some(onboarding) = self.onboarding_service.clone() {
                    self.set_onboarding(onboarding);
                } else {
                    self.items
                        .push(Item::Error("connection wizard is unavailable".to_owned()));
                }
            }
            UiEvent::QuitRequested => self.quit_requested = true,
            UiEvent::PermissionModeChanged { mode } => {
                self.update_welcome_permission(mode.as_str().to_owned());
                if *mode == heycode_agent::ApprovalPolicyKind::AcceptedEdits
                    && let Some(id) = self.pending_edit_approval_id.take()
                    && self.pending_ask.as_ref().is_some_and(|ask| ask.id == id)
                {
                    self.resolve_ask_with(heycode_agent::AskAnswer::Allow);
                    while self.pending_ask.as_ref().is_some_and(|ask| {
                        !self.runtime_permission_ids.contains_key(&ask.id)
                            && heycode_agent::AcceptedEdits::allows_file_tool(&ask.name)
                    }) {
                        self.resolve_ask_with(heycode_agent::AskAnswer::Allow);
                    }
                }
            }
            UiEvent::PlanReviewRequested { id, plan } => {
                self.close_permission_picker();
                self.pending_plan_review =
                    Some(crate::plan_review::PlanReviewView::new(*id, plan.clone()));
            }
            UiEvent::PlanReviewResolved { id } => {
                if self
                    .pending_plan_review
                    .as_ref()
                    .is_some_and(|view| view.id == *id)
                {
                    self.pending_plan_review = None;
                }
            }
            UiEvent::ApprovalRequested {
                id,
                name,
                args_preview,
                owner_session,
            } => {
                self.refresh_tasks();
                let mut ask = PendingAskView::new(*id, name.clone(), args_preview.clone());
                ask.owner_session = owner_session.clone();
                if let Some(session) = owner_session {
                    let row = self
                        .task_console
                        .records
                        .iter()
                        .find(|row| row.session.as_ref() == Some(session));
                    ask.owner_label = Some(
                        row.map_or_else(|| "Child conversation".into(), |row| row.label.clone()),
                    );
                    ask.owner_key = row.map(|row| row.key.clone());
                }
                self.open_ask(ask);
            }
            UiEvent::ApprovalResolved { id, allowed } => {
                let tool_index = self
                    .pending_ask
                    .as_ref()
                    .filter(|ask| ask.id == *id)
                    .or_else(|| self.queued_asks.iter().find(|ask| ask.id == *id))
                    .and_then(|ask| ask.tool_index);
                self.set_tool_approval(
                    tool_index,
                    if *allowed { "approved" } else { "cancelled" }.into(),
                );
                self.queued_asks.retain(|queued| queued.id != *id);
                if self.pending_ask.as_ref().is_some_and(|p| p.id == *id) {
                    self.pending_ask = self.queued_asks.pop_front();
                }
            }
            UiEvent::RuntimePermissionRequested {
                request_id,
                action,
                detail,
            } => {
                // The native runtime mirrors every `ApprovalRequested` onto its
                // event stream as `approval-<id>` for SDK clients. This shell
                // already received the original on the agent's UI bus, so the
                // mirror must not open a second card for the same decision.
                if request_id.starts_with("approval-")
                    || (request_id.starts_with("agent-request-") && self.approvals.is_some())
                {
                    return;
                }
                let id = self.next_runtime_permission_id;
                self.next_runtime_permission_id = self.next_runtime_permission_id.saturating_sub(1);
                self.runtime_permission_ids.insert(id, request_id.clone());
                self.open_ask(PendingAskView::new(id, action.clone(), detail.clone()));
            }
            UiEvent::OptionalQuestionRequested {
                session_id,
                question_id,
                prompt,
                choices,
                mode,
                header,
                choice_descriptions,
            } => {
                self.apply_optional_question(session_id, question_id, prompt, choices);
                if let Some(row) =
                    self.optional_questions.rows.iter_mut().find(|row| {
                        row.session_id == *session_id && row.question_id == *question_id
                    })
                {
                    row.mode = *mode;
                    row.header.clone_from(header);
                    row.choice_descriptions.clone_from(choice_descriptions);
                }
            }
            UiEvent::OptionalQuestionSettled {
                session_id,
                question_id,
            } => {
                self.settle_optional_question(session_id, question_id);
            }
            UiEvent::RuntimeQuestionRequested {
                request_id,
                header,
                prompt,
                choices,
                choice_descriptions,
                mode,
                progress,
            } => {
                self.close_capability_catalog();
                self.profile_picker = None;
                self.session_browser = None;
                self.theme_picker = None;
                self.advisor_panel = None;
                self.rewind_picker = None;
                self.keymap_picker = None;
                self.help_panel = None;
                self.memory_panel = None;
                self.skill_doctor_panel = None;
                self.copy_panel = None;
                self.scroll_speed_picker = None;
                self.command_palette = None;
                self.close_permission_picker();
                self.close_model_picker();
                self.close_effort_picker();
                self.close_route_picker();
                self.close_mcp_panel();
                self.close_plugin_panel();
                self.pending_runtime_question = Some(PendingRuntimeQuestionView {
                    mode: *mode,
                    progress: *progress,
                    selected_choices: Default::default(),
                    request_id: request_id.clone(),
                    header: header.clone(),
                    prompt: prompt.clone(),
                    choices: choices.clone(),
                    choice_descriptions: choice_descriptions.clone(),
                    selection: 0,
                    input: String::new(),
                });
            }
            UiEvent::InboxUpdated {
                next_turn,
                next_step,
                wake,
            } => {
                self.inbox_pending = heycode_agent::InboxPending {
                    next_turn: *next_turn,
                    next_step: *next_step,
                };
                if *next_turn == 0 && *next_step == 0 {
                    self.follow_up_wake_pending = false;
                } else if *wake == heycode_agent::InboxWake::Wake {
                    self.follow_up_wake_pending = true;
                }
            }
        }
    }

    /// Rebuild transcript items from a resumed session's durable events so
    /// `--continue` opens with full history instead of an empty screen.
    pub fn replay(&mut self, events: &[heycode_session::SessionEvent]) {
        self.tool_group_signature = None;
        self.items.clear();
        self.inbox_transcript = inbox_transcript::InboxTranscript::default();
        self.workflow_console.inbox_next_turn.clear();
        self.workflow_console.inbox_next_step.clear();
        self.workflow_console.claimed_workflow_notice = None;
        self.tool_items.clear();
        self.server_tool_items.clear();
        self.live_assistant_item.clear();
        self.live_reasoning_item.clear();
        self.last_request_route = None;
        self.reasoning_focus = None;
        self.reasoning_hit_rows.clear();
        self.context_budget = None;
        let mut phases: HashMap<(u64, u32), (i64, Option<i64>)> = HashMap::new();
        let committed_messages: std::collections::BTreeSet<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                heycode_session::SessionEventKind::AssistantMessage { turn, step, .. } => {
                    Some((*turn, *step))
                }
                _ => None,
            })
            .collect();
        for event in events {
            use heycode_session::SessionEventKind as K;
            if let K::AssistantChunk {
                turn,
                step,
                text,
                reasoning,
            } = &event.kind
            {
                if reasoning
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    phases
                        .entry((*turn, *step))
                        .or_insert((event.time_ms, None));
                }
                if text.as_ref().is_some_and(|value| !value.is_empty())
                    && let Some((_, end)) = phases.get_mut(&(*turn, *step))
                {
                    end.get_or_insert(event.time_ms);
                }
            }
            let before = self.items.len();
            // Error/crash paths can retain chunks without a final message.
            // Rebuild only those incomplete streams for inspection; finalized
            // streams still use their authoritative message exactly once.
            let partial_chunk = matches!(&event.kind, K::AssistantChunk { turn, step, .. }
                if !committed_messages.contains(&(*turn, *step)));
            self.apply_durable_event(event, partial_chunk);
            if partial_chunk {
                for item in &mut self.items[before..] {
                    if let Item::Reasoning { view, .. } = item {
                        view.started = None;
                        view.elapsed_seconds = None;
                    }
                }
            }
            if let K::AssistantMessage { turn, step, .. } = &event.kind
                && let Some((start, end)) = phases.remove(&(*turn, *step))
            {
                let duration = end
                    .unwrap_or(event.time_ms)
                    .checked_sub(start)
                    .and_then(|millis| u64::try_from(millis).ok())
                    .map(|millis| millis / 1000);
                for item in &mut self.items[before..] {
                    if let Item::Reasoning { view, .. } = item {
                        view.elapsed_seconds = duration;
                    }
                }
            }
        }
        // Opening a log cannot resurrect an unfinished producer. Do not
        // relabel a later, fully committed turn because an older one was partial.
        for index in self.live_reasoning_item.values() {
            if let Some(Item::Reasoning { done, view, .. }) = self.items.get_mut(*index)
                && !*done
            {
                *done = true;
                view.finish(true);
            }
        }
        self.live_assistant_item.clear();
        self.live_reasoning_item.clear();
    }

    /// Apply one post-commit session event to the live transcript.
    ///
    /// Unlike [`Self::replay`], presentational chunks animate until their
    /// correlated complete assistant message replaces them. Provider-owned
    /// JSON is never inspected; only normalized event fields become cards.
    pub fn apply_session_event(&mut self, event: &heycode_session::SessionEvent) {
        self.apply_durable_event(event, true);
    }

    pub(crate) fn receive_mcp_event(&mut self, event: crate::product_attachments::McpTuiEvent) {
        match event {
            crate::product_attachments::McpTuiEvent::Elicitation { id, request } => {
                if self.pending_mcp_elicitation.is_none() {
                    self.open_mcp_elicitation(id, request);
                } else {
                    self.queued_mcp_elicitations.push_back((id, request));
                }
            }
            crate::product_attachments::McpTuiEvent::Human(event) => match event {
                heycode_mcp::McpClientEvent::Progress(progress) => {
                    let total = progress
                        .total()
                        .map_or_else(String::new, |total| format!("/{total}"));
                    let message = progress.message().map_or_else(String::new, |message| {
                        format!(" · {}", terminal_safe_human(message, 4 * 1024))
                    });
                    self.items.push(Item::Info(format!(
                        "MCP {} progress {}{total}{message}",
                        progress.server().as_str(),
                        progress.progress()
                    )));
                }
                heycode_mcp::McpClientEvent::Log(log) => {
                    let logger = log.logger().map_or_else(String::new, |logger| {
                        format!(" {}", terminal_safe_human(logger, 256))
                    });
                    let data = serde_json::to_string(log.data()).map_or_else(
                        |_| "<invalid log>".to_owned(),
                        |value| terminal_safe_human(&value, 4 * 1024),
                    );
                    self.items.push(Item::Info(format!(
                        "MCP {} {}{logger}: {data}",
                        log.server().as_str(),
                        log.level().as_str()
                    )));
                }
                heycode_mcp::McpClientEvent::ElicitationComplete { server, .. } => {
                    self.items.push(Item::Info(format!(
                        "MCP {} URL elicitation completed",
                        server.as_str()
                    )));
                }
            },
        }
    }

    fn open_mcp_elicitation(&mut self, id: u64, request: heycode_mcp::McpElicitationRequest) {
        self.close_capability_catalog();
        self.profile_picker = None;
        self.session_browser = None;
        self.theme_picker = None;
        self.advisor_panel = None;
        self.rewind_picker = None;
        self.keymap_picker = None;
        self.help_panel = None;
        self.memory_panel = None;
        self.skill_doctor_panel = None;
        self.copy_panel = None;
        self.scroll_speed_picker = None;
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        let mode = match request.mode() {
            heycode_mcp::McpElicitationMode::Form { schema } => PendingMcpElicitationMode::Form {
                schema: schema.value().clone(),
                input: "{}".to_owned(),
                error: None,
            },
            heycode_mcp::McpElicitationMode::Url { url, .. } => {
                PendingMcpElicitationMode::Url { url: url.clone() }
            }
        };
        self.pending_mcp_elicitation = Some(PendingMcpElicitationView {
            id,
            server: request.server().as_str().to_owned(),
            message: request.message().to_owned(),
            mode,
        });
    }

    fn resolve_mcp_elicitation(&mut self, accept: bool) {
        let Some(mut view) = self.pending_mcp_elicitation.take() else {
            return;
        };
        let response = if !accept {
            Ok(heycode_mcp::McpElicitationResponse::decline())
        } else {
            match &mut view.mode {
                PendingMcpElicitationMode::Form { input, error, .. } => {
                    match serde_json::from_str::<serde_json::Value>(input) {
                        Ok(value) if value.is_object() => {
                            Ok(heycode_mcp::McpElicitationResponse::accept(value))
                        }
                        Ok(_) | Err(_) => {
                            *error = Some("enter one valid JSON object".to_owned());
                            self.pending_mcp_elicitation = Some(view);
                            return;
                        }
                    }
                }
                PendingMcpElicitationMode::Url { .. } => {
                    Ok(heycode_mcp::McpElicitationResponse::accept_url())
                }
            }
        };
        self.pending_mcp_elicitation_response = Some((view.id, response));
        if let Some((id, request)) = self.queued_mcp_elicitations.pop_front() {
            self.open_mcp_elicitation(id, request);
        }
    }

    pub(crate) fn take_mcp_elicitation_response(
        &mut self,
    ) -> Option<(
        u64,
        Result<heycode_mcp::McpElicitationResponse, heycode_mcp::McpElicitationFailure>,
    )> {
        self.pending_mcp_elicitation_response.take()
    }

    fn apply_durable_event(&mut self, event: &heycode_session::SessionEvent, live: bool) {
        use heycode_session::SessionEventKind as K;
        self.inbox_transcript.observe(&event.kind);
        self.items.extend(
            self.inbox_transcript
                .take_agent_receipts()
                .into_iter()
                .filter_map(inbox_transcript::agent_receipt_item),
        );
        if !live && let Some(growth) = heycode_session::retained_context_growth(&event.kind) {
            self.restore_context_growth(growth.tokens, growth.uncounted);
        }
        match &event.kind {
            K::RuntimeLinked { runtime, .. } => self.items.push(Item::RuntimeLink {
                runtime: runtime.clone(),
            }),
            K::RuntimeConfigured {
                state,
                model,
                reasoning_effort,
                ..
            } if state.is_committed() => {
                if let Some(model) = model {
                    self.set_active_model_id(model.clone());
                    if let Some(welcome) = self.welcome.as_mut() {
                        welcome.model.clone_from(model);
                    }
                }
                self.set_configured_reasoning_effort(reasoning_effort.clone());
            }
            K::RuntimeConfigured { .. } => {}
            K::RequestContext { context, .. } => {
                if let Some(budget) = context.budget.as_deref() {
                    self.apply(&UiEvent::ContextBudgetChanged {
                        budget: budget.clone(),
                    });
                }
            }
            K::RequestHeader { header, .. } => {
                let route = (header.provider.clone(), header.model.clone());
                if self
                    .last_request_route
                    .as_ref()
                    .is_some_and(|previous| previous != &route)
                {
                    self.items.push(Item::RouteChange {
                        provider: route.0.clone(),
                        model: route.1.clone(),
                    });
                }
                self.last_request_route = Some(route);
            }
            K::UserAttachments {
                attachments,
                document_routes,
            } => self.items.push(Item::Attachments {
                attachments: attachments.clone(),
                document_routes: document_routes.clone(),
            }),
            K::UserMessage { text } => {
                let operational = self.inbox_transcript.take_operational(text);
                if !self.workflow_console.consume_workflow_notice(text) {
                    if matches!(
                        operational,
                        Some(inbox_transcript::OperationalMessage::AgentCompletionDelivered)
                    ) {
                        // The durable insertion already produced the one named
                        // receipt. Model admission retains its exact input only.
                    } else if let Some(inbox_transcript::OperationalMessage::AgentMessage(
                        message,
                    )) = operational
                    {
                        self.items
                            .push(inbox_transcript::agent_message_item(message));
                    } else if let Some(inbox_transcript::OperationalMessage::Job(job)) = operational
                    {
                        let outcome = ["completed", "failed", "cancelled", "interrupted"]
                            .into_iter()
                            .find(|status| {
                                inbox_transcript::job_notice_has_status(text, &job, status)
                            });
                        // Forks inherit transcript notices, but their new job
                        // registry may reuse the parent's local job numbers.
                        let completed_job = (event.seq >= self.workflow_console.first_local_seq
                            && inbox_transcript::job_notice_has_status(text, &job, "completed"))
                        .then(|| job.clone());
                        self.items.push(Item::Tool {
                            call_id: None,
                            name: "job".into(),
                            args: serde_json::json!({ "job_id": job, "outcome": outcome }),
                            result: Some((
                                outcome == Some("completed"),
                                serde_json::Value::String(text.clone()),
                            )),
                            untrusted_content: None,
                            view: ToolViewState {
                                completed_job,
                                ..ToolViewState::default()
                            },
                        });
                    } else if let Some(
                        inbox_transcript::OperationalMessage::OptionalQuestionAnswer(answer),
                    ) = operational
                    {
                        self.items
                            .push(Item::User(format!("Answer to optional question: {answer}")));
                    } else if let Some(inbox_transcript::OperationalMessage::ScheduleReminder(
                        prompt,
                    )) = operational
                    {
                        self.items
                            .push(Item::Info(format!("Scheduled reminder: {prompt}")));
                    } else {
                        self.items.push(Item::User(text.clone()));
                    }
                }
            }
            K::AgentInboxSplice { .. } => {
                if event.seq < self.workflow_console.first_local_seq {
                    return;
                }
                let routine_jobs = self
                    .task_console
                    .records
                    .iter()
                    .filter(|record| {
                        matches!(
                            record.status,
                            crate::task_console::TaskStatus::Completed
                                | crate::task_console::TaskStatus::Cancelled
                        )
                    })
                    .filter_map(|record| record.job.clone())
                    .collect();
                self.workflow_console
                    .observe_inbox_splice(&event.kind, &routine_jobs);
            }
            K::HookContribution { contribution } => {
                self.items.push(Item::Info(format!(
                    "hook {} · {:?}/{:?} · {:?}\n{}",
                    contribution.owner(),
                    contribution.phase(),
                    contribution.event(),
                    contribution.handler(),
                    contribution.render_for_model()
                )));
            }
            K::AssistantChunk {
                turn,
                step,
                text,
                reasoning,
            } if live => {
                if text.as_ref().is_some_and(|text| !text.is_empty()) {
                    self.finish_reasoning(false);
                }
                if let Some(text) = text {
                    let index = self.live_assistant_item.get(&(*turn, *step)).copied();
                    match index.and_then(|index| self.items.get_mut(index)) {
                        Some(Item::Assistant(buffer)) => buffer.push_str(text),
                        _ => {
                            let index = self.items.len();
                            self.items.push(Item::Assistant(text.clone()));
                            self.live_assistant_item.insert((*turn, *step), index);
                        }
                    }
                }
                if let Some(reasoning) = reasoning {
                    let index = self.live_reasoning_item.get(&(*turn, *step)).copied();
                    match index.and_then(|index| self.items.get_mut(index)) {
                        Some(Item::Reasoning { text, .. }) => text.push_str(reasoning),
                        _ if !reasoning.trim().is_empty() => {
                            let index = self.items.len();
                            self.items.push(Item::Reasoning {
                                text: reasoning.clone(),
                                done: false,
                                view: ReasoningView::live(),
                            });
                            self.live_reasoning_item.insert((*turn, *step), index);
                        }
                        _ => {}
                    }
                }
            }
            K::AssistantChunk { .. } => {}
            K::AssistantMessage {
                turn,
                step,
                content,
                reasoning,
                usage,
                ..
            } => {
                if let Some(reasoning) = reasoning.as_ref().filter(|value| !value.trim().is_empty())
                {
                    if let Some(index) = self.live_reasoning_item.remove(&(*turn, *step)) {
                        if let Some(Item::Reasoning { text, done, view }) =
                            self.items.get_mut(index)
                        {
                            *text = reasoning.clone();
                            *done = true;
                            view.finish(false);
                        }
                    } else {
                        self.items.push(Item::Reasoning {
                            text: reasoning.clone(),
                            done: true,
                            view: Default::default(),
                        });
                    }
                }
                if !content.trim().is_empty() {
                    if let Some(index) = self.live_assistant_item.remove(&(*turn, *step)) {
                        if let Some(Item::Assistant(text)) = self.items.get_mut(index) {
                            *text = content.clone();
                        }
                    } else {
                        self.items.push(Item::Assistant(content.clone()));
                    }
                }
                self.usage = *usage;
            }
            K::AssistantAudio { attachments, .. } => {
                self.finish_reasoning(false);
                self.items.push(Item::AudioOutput {
                    attachments: attachments.clone(),
                });
            }
            // Provider continuation state belongs to persistence, not the transcript.
            K::AssistantProviderItem { .. } => {}
            K::ServerToolCall { call, .. } => {
                let index = self.items.len();
                self.items.push(Item::ServerTool {
                    call_id: call.id().clone(),
                    logical: call.logical().to_owned(),
                    provider_name: call.provider_name().to_owned(),
                    result: None,
                });
                self.server_tool_items
                    .insert(call.id().as_str().to_owned(), index);
            }
            K::ServerToolResult { result, .. } => {
                if let Some(index) = self
                    .server_tool_items
                    .get(result.call_id().as_str())
                    .copied()
                    && let Some(Item::ServerTool {
                        result: settlement, ..
                    }) = self.items.get_mut(index)
                {
                    *settlement = Some(result.as_ref().clone());
                }
            }
            K::ServerToolUsage { usage, .. } => {
                let cost = match usage.cost() {
                    heycode_core::ServerToolUsageCost::Unknown => "cost unknown".to_owned(),
                    heycode_core::ServerToolUsageCost::Published(cost) => {
                        format!("{} {} pico-units", cost.currency(), cost.pico_units())
                    }
                };
                self.items.push(Item::ServerToolUsage {
                    logical: usage.logical().to_owned(),
                    requests: usage.requests(),
                    cost,
                });
            }
            K::AssistantCitation { citation, .. } => self.items.push(Item::Citation {
                url: citation.url().to_owned(),
                title: citation.title().map(str::to_owned),
                cited_text: citation.cited_text().map(str::to_owned),
                start_index: citation.start_index(),
                end_index: citation.end_index(),
            }),
            K::ToolCall {
                call_id,
                name,
                args,
                ..
            } => {
                self.finish_reasoning(false);
                let index = self.items.len();
                self.items.push(Item::Tool {
                    call_id: Some(call_id.clone()),
                    name: name.clone(),
                    args: args.clone(),
                    result: None,
                    untrusted_content: None,
                    view: ToolViewState::default(),
                });
                self.tool_items.insert(call_id.as_str().to_owned(), index);
                self.bind_pending_tool_approval(index);
            }
            K::ToolResult {
                call_id,
                content,
                is_error,
                untrusted_content,
            } => {
                self.settle_tool(
                    call_id,
                    !*is_error,
                    heycode_session::tool_result_value(content),
                    *untrusted_content,
                );
            }
            K::RichToolResult {
                call_id,
                result,
                is_error,
                untrusted_content,
            } => self.settle_tool(
                call_id,
                !*is_error,
                serde_json::to_value(result.as_ref())
                    .unwrap_or_else(|_| serde_json::Value::String(result.render_for_model())),
                *untrusted_content,
            ),
            K::CompactionApplied {
                summary,
                replaced_upto_seq,
            } => self.items.push(Item::Compaction {
                native: false,
                strategy: None,
                replaced_upto_seq: *replaced_upto_seq,
                summary: Some(summary.clone()),
                provider_items: 0,
                expanded: false,
                focused: false,
            }),
            K::NativeCompactionApplied {
                strategy,
                replaced_upto_seq,
                items,
                ..
            } => self.items.push(Item::Compaction {
                native: true,
                strategy: Some(strategy.clone()),
                replaced_upto_seq: *replaced_upto_seq,
                summary: None,
                provider_items: items.len(),
                expanded: false,
                focused: false,
            }),
            K::PlanReview { .. } => {}
            K::ReviewChange { change } => {
                if let heycode_session::ReviewChange::FindingsReported { report, .. } =
                    change.as_ref()
                {
                    self.items.push(Item::FindingsReport {
                        report: report.clone(),
                        expanded: false,
                        focused: false,
                    });
                    self.scroll_from_bottom = 0;
                }
            }
            K::PlanMode { active } => self.items.push(Item::PlanMode { active: *active }),
            K::GoalChange { change } => match change.as_ref() {
                heycode_session::GoalChange::Snapshot { action, goal, .. } => {
                    self.items.push(Item::Goal {
                        action: format!("{action:?}").to_ascii_lowercase(),
                        phase: Some(format!("{:?}", goal.phase()).to_ascii_lowercase()),
                        objective: Some(goal.objective().to_owned()),
                        revision: goal.revision(),
                    });
                }
                heycode_session::GoalChange::Clear { cleared, .. } => {
                    self.items.push(Item::Goal {
                        action: "clear".to_owned(),
                        phase: None,
                        objective: None,
                        revision: cleared.revision(),
                    });
                }
            },
            K::CodeModeChange { change } => {
                use heycode_session::CodeModeChange;
                let (action, summary) = match change.as_ref() {
                    CodeModeChange::Saved { script } => ("script saved", script.name.clone()),
                    CodeModeChange::Started { run_id, script } => {
                        ("script start", format!("{} · {run_id}", script.name))
                    }
                    CodeModeChange::CallStarted {
                        run_id, call, name, ..
                    } => (
                        "script call",
                        format!("{run_id} · call {call} · {name} · awaiting result"),
                    ),
                    CodeModeChange::CallFinished {
                        run_id,
                        call,
                        error,
                        ..
                    } => (
                        "script result",
                        format!(
                            "{run_id} · call {call} · {}",
                            if error.is_some() {
                                "failed"
                            } else {
                                "completed"
                            }
                        ),
                    ),
                    CodeModeChange::Finished { run_id, error, .. } => (
                        "script end",
                        format!(
                            "{run_id} · {}",
                            if error.is_some() {
                                "failed; inspect run status for preceding effects"
                            } else {
                                "completed"
                            }
                        ),
                    ),
                };
                self.items.push(Item::Workflow {
                    action: action.into(),
                    summary,
                });
            }
            K::WorkflowChange { change } => {
                // Structured progress lives in the persistent rail and workspace.
                // Retained transcript events still support legacy detached shells.
                if self.workflow_console.attached() {
                    self.workflow_console.refresh();
                    return;
                }
                let (action, summary) = match change.as_ref() {
                    heycode_session::WorkflowChange::Agent { .. }
                    | heycode_session::WorkflowChange::Job { .. } => return,
                    heycode_session::WorkflowChange::Saved { definition, .. } => (
                        "saved",
                        format!("{} · {} steps", definition.name(), definition.steps().len()),
                    ),
                    heycode_session::WorkflowChange::Node {
                        step_id, record, ..
                    } => (
                        "node",
                        format!(
                            "{step_id} · {:?} · attempt {}",
                            record.state, record.attempt
                        ),
                    ),
                    heycode_session::WorkflowChange::Start { definition, .. } => (
                        "start",
                        format!("{} · {} steps", definition.name(), definition.steps().len()),
                    ),
                    heycode_session::WorkflowChange::Progress { step, message, .. } => {
                        ("progress", format!("step {step} · {message}"))
                    }
                    heycode_session::WorkflowChange::Checkpoint {
                        completed_steps, ..
                    } => ("checkpoint", format!("{completed_steps} steps complete")),
                    heycode_session::WorkflowChange::Resume {
                        attempt,
                        completed_steps,
                        ..
                    } => (
                        "resume",
                        format!("attempt {attempt} · {completed_steps} steps complete"),
                    ),
                    heycode_session::WorkflowChange::End {
                        outcome,
                        completed_steps,
                        message,
                        ..
                    } => (
                        "end",
                        format!(
                            "{} · {completed_steps} steps complete{}",
                            format!("{outcome:?}").to_ascii_lowercase(),
                            message
                                .as_deref()
                                .map_or_else(String::new, |message| format!(" · {message}"))
                        ),
                    ),
                };
                self.items.push(Item::Workflow {
                    action: action.to_owned(),
                    summary,
                });
            }
            K::ScheduleChange { change } => {
                let (action, summary) = match change.as_ref() {
                    heycode_session::ScheduleChange::Create { schedule, .. } => (
                        "create",
                        format!(
                            "{} · at {} · {:?}",
                            schedule.id(),
                            schedule.scheduled_at_ms(),
                            schedule.rule()
                        ),
                    ),
                    heycode_session::ScheduleChange::Delete { id, .. } => {
                        ("delete", id.to_string())
                    }
                    heycode_session::ScheduleChange::Dispatch {
                        id, accepted_at_ms, ..
                    } => ("dispatch", format!("{id} · accepted at {accepted_at_ms}")),
                    heycode_session::ScheduleChange::Reschedule {
                        id,
                        delay_ms,
                        scheduled_at_ms,
                        ..
                    } => (
                        "reschedule",
                        format!("{id} · at {scheduled_at_ms} · delay {delay_ms}ms"),
                    ),
                };
                self.items.push(Item::Schedule {
                    action: action.to_owned(),
                    summary,
                });
            }
            K::TurnStart { .. } => self.turn_error = TurnErrorRow::None,
            K::TurnEnd { reason, .. } if *reason == heycode_session::TurnEndReason::Error => {
                self.finish_reasoning(true);
                if self.turn_error == TurnErrorRow::None {
                    self.items
                        .push(Item::Error("turn ended with an error".to_owned()));
                    self.turn_error = TurnErrorRow::Generic(self.items.len() - 1);
                }
            }
            K::TurnEnd { reason, .. } if *reason == heycode_session::TurnEndReason::Aborted => {
                self.finish_reasoning(true);
                self.items.push(Item::Info("Cancelled".to_owned()));
            }
            K::SessionCreated { creation } => {
                // A fork announces its lineage the moment it is shown, so
                // `/fork` is never a silent transition into an identical-looking
                // transcript.
                if let Some(parent) = creation.parent() {
                    self.items.push(Item::Info(format!(
                        "forked from session {} at event {}",
                        short_session_id(parent.parent_session_id().as_str()),
                        parent.seed_event_count()
                    )));
                }
            }
            K::SessionTitle { title } => {
                self.current_session_title = Some(title.clone());
            }
            K::TurnEnd { .. } => self.finish_reasoning(false),
            K::StepStart { .. }
            | K::StepEnd { .. }
            | K::TeamChange { .. }
            | K::WorkChange { .. }
            | K::AttachmentAdded { .. }
            | K::AssistantResponseMetadata { .. }
            | K::SessionActivated {} => {}
        }
    }

    fn settle_tool(
        &mut self,
        call_id: &heycode_core::CallId,
        ok: bool,
        value: serde_json::Value,
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    ) {
        self.tool_group_signature = None;
        let Some(index) = self.tool_items.get(call_id.as_str()).copied() else {
            return;
        };
        if let Some(Item::Tool {
            result,
            untrusted_content: boundary,
            ..
        }) = self.items.get_mut(index)
        {
            *result = Some((ok, value));
            *boundary = untrusted_content;
        }
        self.merge_job_output(index);
    }

    fn merge_job_output(&mut self, index: usize) {
        let Some(Item::Tool {
            name,
            args,
            result: Some((true, value)),
            ..
        }) = self.items.get(index)
        else {
            return;
        };
        let canonical = name.strip_prefix("mcp__heycode__").unwrap_or(name);
        if canonical != "job_output"
            && !(canonical == "job_control"
                && args.get("action").and_then(serde_json::Value::as_str) == Some("output"))
        {
            return;
        }
        let Some(job_id) = args.get("job_id").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(text) = value
            .pointer("/page/text")
            .or_else(|| value.get("text"))
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let text = text.to_owned();
        let stream = args
            .get("stream")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(
                if value.get("terminal_id").is_some_and(|id| !id.is_null()) {
                    "terminal"
                } else {
                    "stdout"
                },
            )
            .to_owned();
        let offset = value
            .pointer("/page/offset")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let marker = format!("[inline output capped; use job_output {job_id}]\n");
        let canonical_marker =
            format!("[inline output capped; use job_control action=output job_id={job_id}]\n");
        let direct = self.items[..index].iter().rposition(|item| match item {
            Item::Tool {
                name,
                result: Some((_, value)),
                ..
            } if matches!(
                name.as_str(),
                "bash" | "background_shell" | "background_terminal" | "run_tool"
            ) =>
            {
                value.get("job_id").and_then(serde_json::Value::as_str) == Some(job_id)
                    || value.as_str().is_some_and(|text| {
                        text.starts_with(&marker) || text.starts_with(&canonical_marker)
                    })
            }
            _ => false,
        });
        let target = direct;
        let Some(target) = target else {
            return;
        };
        if let Item::Tool { view, .. } = &mut self.items[target] {
            view.retrieved_output.insert((stream, offset), text);
        }
        if let Item::Tool { view, .. } = &mut self.items[index] {
            view.merged = true;
        }
        if self.reasoning_focus == Some(index) {
            self.focus_reasoning_item(Some(target));
        }
    }

    fn restore_context_growth(&mut self, added: u64, uncounted: bool) {
        if let Some(budget) = &mut self.context_budget {
            budget.used = budget.used.saturating_add(added);
            budget.projected = true;
            if uncounted {
                budget.confidence = heycode_llm::ContextConfidence::AtLeast;
            } else if budget.confidence == heycode_llm::ContextConfidence::Exact {
                budget.confidence = heycode_llm::ContextConfidence::Estimated;
            }
            self.context_tokens = Some(budget.used);
            self.context_tokens_estimated = true;
        }
    }

    fn finish_reasoning(&mut self, interrupted: bool) {
        for item in &mut self.items {
            if let Item::Reasoning { done, view, .. } = item
                && !*done
            {
                *done = true;
                view.finish(interrupted);
            }
        }
        if interrupted {
            // A cancelled stream may commit its partial AssistantMessage before
            // TurnEnd. That commit closes the disclosure but is not completion.
            // Only relabel the trailing reasoning phase; an answer/tool or new
            // user message proves that an earlier phase already finished.
            for item in self.items.iter_mut().rev() {
                match item {
                    Item::Reasoning { view, .. } => {
                        view.interrupted = true;
                        break;
                    }
                    Item::User(_) | Item::Tool { .. } | Item::ServerTool { .. } => break,
                    Item::Assistant(text) if !text.is_empty() => break,
                    _ => {}
                }
            }
        }
    }

    fn focus_reasoning_item(&mut self, index: Option<usize>) {
        if let Some(previous) = self.reasoning_focus
            && let Some(Item::Tool { view, .. }) = self.items.get_mut(previous)
        {
            view.focused = false;
        }
        if let Some(previous) = self.reasoning_focus
            && let Some(Item::Reasoning { view, .. }) = self.items.get_mut(previous)
        {
            view.focused = false;
        }
        if let Some(previous) = self.reasoning_focus
            && let Some(Item::FindingsReport { focused, .. } | Item::Compaction { focused, .. }) =
                self.items.get_mut(previous)
        {
            *focused = false;
        }
        self.reasoning_focus = index;
        if let Some(index) = index
            && let Some(Item::Tool { view, .. }) = self.items.get_mut(index)
        {
            view.focused = true;
            let row = self
                .reasoning_hit_rows
                .iter()
                .find(|(_, item)| *item == index)
                .map_or(0, |(row, _)| {
                    usize::from(row.saturating_sub(self.transcript_area.y))
                });
            self.reasoning_reveal = Some((index, row));
        }
        if let Some(index) = index
            && let Some(Item::Reasoning { view, .. }) = self.items.get_mut(index)
        {
            view.focused = true;
            let row = self
                .reasoning_hit_rows
                .iter()
                .find(|(_, item)| *item == index)
                .map_or(0, |(row, _)| {
                    usize::from(row.saturating_sub(self.transcript_area.y))
                });
            self.reasoning_reveal = Some((index, row));
        }
        if let Some(index) = index
            && let Some(Item::FindingsReport { focused, .. } | Item::Compaction { focused, .. }) =
                self.items.get_mut(index)
        {
            *focused = true;
            let row = self
                .reasoning_hit_rows
                .iter()
                .find(|(_, item)| *item == index)
                .map_or(0, |(row, _)| {
                    usize::from(row.saturating_sub(self.transcript_area.y))
                });
            self.reasoning_reveal = Some((index, row));
        }
    }

    fn toggle_reasoning_item(&mut self, index: usize) {
        self.tool_group_signature = None;
        self.focus_reasoning_item(Some(index));
        if let Some(Item::Tool { view, .. }) = self.items.get_mut(index) {
            view.expanded = !view.expanded;
            self.transcript_cache.invalidate_layout();
        }
        if let Some(Item::Reasoning { view, .. }) = self.items.get_mut(index) {
            view.expanded = Some(!view.expanded.unwrap_or(self.show_reasoning));
            self.transcript_cache.invalidate_layout();
        }
        if let Some(Item::FindingsReport { expanded, .. } | Item::Compaction { expanded, .. }) =
            self.items.get_mut(index)
        {
            *expanded = !*expanded;
            self.transcript_cache.invalidate_layout();
        }
    }

    /// Advance the spinner while busy (busy tick only).
    ///
    /// A terminal that cannot address the cursor cannot animate: redrawing a
    /// changing glyph there emits a new line per tick instead of replacing one.
    pub fn tick_spinner(&mut self) {
        for item in &mut self.items {
            if let Item::Reasoning {
                done: false, view, ..
            } = item
                && let Some(started) = view.started
            {
                view.elapsed_seconds = Some(started.elapsed().as_secs());
            }
        }
        if (self.verb.is_none() && self.side_command_activity.is_none()) || !self.chrome.animation()
        {
            return;
        }
        self.spinner = (self.spinner + 1) % SPINNER_FRAMES.len();
        if self.spinner == 0
            && let Some(position) = SPINNER_VERBS
                .iter()
                .position(|v| Some(*v) == self.verb.as_deref())
        {
            let next = (position + 1) % SPINNER_VERBS.len();
            self.verb = Some(SPINNER_VERBS[next].to_owned());
        }
    }

    /// Advance the optional header companion without changing operational state.
    ///
    /// The event loop calls this slowly while idle and alongside the existing
    /// busy redraw while working. Flat and reduced-motion terminals never call it.
    pub fn tick_pet(&mut self) {
        if !self.chrome.animation() || !self.shell_preferences.header_pet() {
            return;
        }
        self.pet_frame = (self.pet_frame + 1) % 24;
    }

    /// Spinner glyph for the current frame.
    #[must_use]
    pub fn spinner_glyph(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()]
    }

    /// Handle a terminal event; returns true when the loop should exit.
    pub fn handle_terminal_event(&mut self, event: &crossterm::event::Event) -> bool {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        if self.handle_optional_question_event(event) {
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && matches!(
                event,
                Event::Key(_) | Event::Mouse(_) | Event::Paste(_) | Event::Resize(_, _)
            )
            && let Some(picker) = self.rewind_picker.as_mut()
        {
            use crate::rewind_picker::RewindPickerOutcome;
            match picker.handle(event) {
                RewindPickerOutcome::None => {}
                RewindPickerOutcome::Cancelled => self.rewind_picker = None,
                RewindPickerOutcome::Submit(selection) => {
                    self.rewind_picker = None;
                    if self
                        .current_session_id()
                        .as_ref()
                        .is_some_and(|id| selection.belongs_to(id))
                    {
                        let command = selection.command();
                        self.queue_command(command.clone(), command);
                    } else {
                        self.items.push(Item::Error(
                            "Rewind checkpoint belongs to another session".to_owned(),
                        ));
                    }
                }
            }
            if !matches!(event, Event::Resize(_, _)) {
                return false;
            }
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.advisor_panel.as_mut()
        {
            use crate::advisor_panel::{AdvisorPanelAction, AdvisorPanelOutcome};
            match panel.handle(event) {
                AdvisorPanelOutcome::Handled => {}
                AdvisorPanelOutcome::Close => self.advisor_panel = None,
                AdvisorPanelOutcome::Commit(action) => {
                    let result = self
                        .advisor_control
                        .as_ref()
                        .ok_or_else(|| "Advisor service unavailable".to_owned())
                        .and_then(|(service, agent)| {
                            match action {
                                AdvisorPanelAction::Disable => service.disable(agent),
                                AdvisorPanelAction::Select(selection) => {
                                    service.select(agent, selection)
                                }
                            }
                            .map_err(|error| error.to_string())
                        });
                    match result {
                        Ok(_) => self.advisor_panel = None,
                        Err(message) => {
                            if let Some(panel) = self.advisor_panel.take() {
                                self.advisor_panel = Some(panel.with_notice(message));
                            }
                        }
                    }
                }
            }
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.skill_doctor_panel.as_mut()
        {
            if matches!(
                panel.handle(event),
                crate::skill_doctor_panel::SkillDoctorPanelAction::Close
            ) {
                self.skill_doctor_panel = None;
            }
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(help) = self.help_panel.as_mut()
        {
            if help.handle(event) {
                self.help_panel = None;
                self.memory_panel = None;
                self.skill_doctor_panel = None;
                self.copy_panel = None;
            }
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.memory_panel.as_mut()
        {
            match panel.handle(event) {
                crate::memory_panel::MemoryPanelAction::None => {}
                crate::memory_panel::MemoryPanelAction::Close => {
                    self.memory_panel = None;
                    self.push_command_receipt(CANCELLED_MEMORY_EDITING);
                }
                crate::memory_panel::MemoryPanelAction::Show(id) => {
                    self.memory_panel = None;
                    self.skill_doctor_panel = None;
                    self.copy_panel = None;
                    if let Some(manager) = self.memory_sources.as_ref() {
                        match crate::memory_commands::memory_show_text(manager, &id) {
                            Ok(text) => self.items.push(Item::Info(text)),
                            Err(error) => self.items.push(Item::Error(error.to_string())),
                        }
                    }
                }
            }
            return false;
        }
        if !self.standard_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(dialog) = self.add_directory_dialog.as_mut()
        {
            match event {
                Event::Key(key) => dialog.key(*key),
                Event::Mouse(mouse) => dialog.mouse(*mouse),
                Event::Paste(text) => dialog.paste(text),
                _ => {}
            }
            return false;
        }
        if !self.standard_priority_modal_open()
            && self.pending_plan_review.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(progress) = self.export_progress.as_mut()
        {
            let _ = progress.handle_event(event);
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.export_panel.as_mut()
        {
            let action = panel.handle_event(event);
            self.apply_export_action(action);
            return false;
        }
        if !self.standard_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && self.add_directory_dialog.is_none()
            && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.copy_panel.as_mut()
        {
            let action = panel.handle_event(event);
            self.apply_copy_action(action);
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && matches!(event, Event::Mouse(_) | Event::Paste(_))
            && let Some(panel) = self.mcp_panel.as_mut()
        {
            match event {
                Event::Mouse(mouse) => {
                    let _ = panel.handle_mouse(*mouse);
                }
                Event::Paste(text) => {
                    let _ = panel.paste(text);
                }
                _ => {}
            }
            return false;
        }
        if !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
            && self.pending_mcp_elicitation.is_none()
            && let Event::Mouse(mouse) = event
            && let Some(panel) = self.settings_panel.as_mut()
        {
            panel.handle_mouse(*mouse);
            return false;
        }
        if self.handle_workflow_event(event) {
            return false;
        }
        if self.handle_task_terminal_event(event) {
            return false;
        }
        // Read-only/lifecycle catalogs are modal surfaces too. Pointer and
        // paste events must not leak through to the transcript or composer
        // behind them. A wheel follows the same currently focused list as the
        // advertised Up/Down keys; clicks remain ordinary terminal selection
        // because these panels do not advertise click activation.
        if let Event::Mouse(mouse) = event {
            let direction = match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp => Some(false),
                crossterm::event::MouseEventKind::ScrollDown => Some(true),
                _ => None,
            };
            if let Some(panel) = self.skills_panel.as_mut() {
                if let Some(forward) = direction {
                    panel.move_selection(if forward { 1 } else { -1 });
                }
                return false;
            }
            if let Some(panel) = self.capability_catalog.as_mut() {
                if let Some(forward) = direction {
                    panel.move_selection(if forward { 1 } else { -1 });
                }
                return false;
            }
            if let Some(panel) = self.plugin_panel.as_mut() {
                if let Some(forward) = direction {
                    let code = if forward { KeyCode::Down } else { KeyCode::Up };
                    let _ = panel.handle_key(code, KeyModifiers::NONE);
                }
                return false;
            }
        }
        if let Event::Paste(text) = event
            && let Some(panel) = self.skills_panel.as_mut()
        {
            panel.paste(text);
            return false;
        }
        if matches!(event, Event::Paste(_))
            && (self.capability_catalog.is_some() || self.plugin_panel.is_some())
        {
            return false;
        }
        if let Event::Mouse(mouse) = event {
            if self.scroll_speed_picker.is_some() {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => self.preview_scroll_speed(1),
                    crossterm::event::MouseEventKind::ScrollDown => self.preview_scroll_speed(-1),
                    _ => {}
                }
                return false;
            }
            if !self.workspace_trust_is_foreground()
                && self.pending_secret.is_none()
                && !self.onboarding.as_ref().is_some_and(|view| view.active)
                && let Some(view) = self.pending_plan_review.as_mut()
            {
                view.handle(event);
                return false;
            }
            match self
                .screen_selection
                .handle(*mouse, std::time::Instant::now())
            {
                Some(crate::screen_selection::SelectionAction::Copy(text)) => {
                    self.pending_clipboard = Some(text)
                }
                Some(crate::screen_selection::SelectionAction::Click(position)) => {
                    if self.mascot_hitbox.contains(position)
                        && self.chrome.animation()
                        && self.shell_preferences.header_pet()
                        && !self.high_priority_modal_open()
                        && self.pending_plan_review.is_none()
                        && self.command_palette.is_none()
                        && self.help_panel.is_none()
                        && self.theme_picker.is_none()
                        && self.session_browser.is_none()
                        && self.settings_panel.is_none()
                    {
                        self.companion.click();
                        return false;
                    }
                    if let Some((_, index)) = self
                        .reasoning_hit_rows
                        .iter()
                        .find(|(row, _)| *row == position.y)
                        .copied()
                        && self.transcript_area.contains(position)
                    {
                        self.toggle_reasoning_item(index);
                    } else {
                        self.focus_reasoning_item(None);
                    }
                }
                None => {}
            }
            if self
                .transcript_area
                .contains(ratatui::layout::Position::new(mouse.column, mouse.row))
            {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        self.scroll_transcript_by_wheel(1);
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        self.scroll_transcript_by_wheel(-1);
                    }
                    _ => {}
                }
            }
            return false;
        }
        if matches!(event, Event::Key(key) if key.code == KeyCode::Esc) {
            self.screen_selection.clear();
        }
        if self.workspace_trust_is_foreground() {
            return self.handle_workspace_trust_event(event);
        }
        if self.pending_secret.is_some() {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Esc => self.cancel_secret_prompt(),
                    KeyCode::Enter => self.submit_secret_prompt(),
                    KeyCode::Backspace => {
                        if let Some(view) = self.pending_secret.as_mut() {
                            view.secret.pop();
                        }
                    }
                    KeyCode::Char(character)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if let Some(view) = self.pending_secret.as_mut()
                            && view.secret.chars().count() < 8_192
                        {
                            view.secret.push(character);
                        }
                    }
                    _ => {}
                }
                return false;
            }
            if let Event::Paste(text) = event
                && let Some(view) = self.pending_secret.as_mut()
            {
                let remaining = 8_192_usize.saturating_sub(view.secret.chars().count());
                view.secret.extend(text.chars().take(remaining));
                return false;
            }
        }
        if self
            .onboarding
            .as_ref()
            .is_some_and(|onboarding| onboarding.active)
            && let Event::Paste(text) = event
        {
            for character in text.chars().take(256) {
                self.apply_onboarding_action(heycode_onboarding::OnboardingAction::Search(
                    character,
                ));
            }
            return false;
        }
        if self
            .onboarding
            .as_ref()
            .is_some_and(|onboarding| onboarding.active)
            && let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                if self.ctrl_c_seen {
                    return true;
                }
                self.ctrl_c_seen = true;
                return false;
            }
            let action = match key.code {
                KeyCode::Up | KeyCode::Left => Some(heycode_onboarding::OnboardingAction::Previous),
                KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                    Some(heycode_onboarding::OnboardingAction::Next)
                }
                KeyCode::Enter => Some(heycode_onboarding::OnboardingAction::Confirm),
                KeyCode::Esc => Some(heycode_onboarding::OnboardingAction::Cancel),
                KeyCode::Backspace => Some(heycode_onboarding::OnboardingAction::Backspace),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(heycode_onboarding::OnboardingAction::ClearInput)
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    Some(heycode_onboarding::OnboardingAction::Search(character))
                }
                _ => None,
            };
            if let Some(action) = action {
                self.apply_onboarding_action(action);
            }
            return self.quit_requested;
        }
        if self.pending_plan_review.is_some()
            && let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && (key.code == KeyCode::BackTab
                || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT)))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.cycle_approval_mode();
            return false;
        }
        if let Some(view) = self.pending_plan_review.as_mut() {
            if let Some(decision) = view.handle(event) {
                let id = view.id;
                if let Some(policy) = &self.approvals {
                    policy.answer_plan(id, decision);
                }
                self.pending_plan_review = None;
            }
            return false;
        }
        if self.pending_mcp_elicitation.is_some() {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Enter => self.resolve_mcp_elicitation(true),
                        KeyCode::Esc => self.resolve_mcp_elicitation(false),
                        KeyCode::Backspace => {
                            if let Some(PendingMcpElicitationView {
                                mode: PendingMcpElicitationMode::Form { input, error, .. },
                                ..
                            }) = self.pending_mcp_elicitation.as_mut()
                            {
                                input.pop();
                                *error = None;
                            }
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            if let Some(PendingMcpElicitationView {
                                mode: PendingMcpElicitationMode::Form { input, error, .. },
                                ..
                            }) = self.pending_mcp_elicitation.as_mut()
                                && input.len() < 16 * 1024
                                && !character.is_control()
                            {
                                input.push(character);
                                *error = None;
                            }
                        }
                        _ => {}
                    }
                    return false;
                }
                Event::Paste(text) => {
                    if let Some(PendingMcpElicitationView {
                        mode: PendingMcpElicitationMode::Form { input, error, .. },
                        ..
                    }) = self.pending_mcp_elicitation.as_mut()
                    {
                        let remaining = (16_usize * 1024).saturating_sub(input.len());
                        input.extend(
                            text.chars()
                                .filter(|character| !character.is_control())
                                .take(remaining),
                        );
                        *error = None;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        if self.pending_runtime_question.is_some() {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let choice_count = self
                        .pending_runtime_question
                        .as_ref()
                        .map_or(0, |question| question.choices.len());
                    let selection_count = if choice_count == 0 {
                        0
                    } else {
                        choice_count + 1
                    };
                    let editing_custom =
                        self.pending_runtime_question
                            .as_ref()
                            .is_some_and(|question| {
                                question.choices.is_empty()
                                    || question.selection == question.choices.len()
                            });
                    match key.code {
                        KeyCode::Up | KeyCode::Left if selection_count > 0 => {
                            if let Some(question) = self.pending_runtime_question.as_mut() {
                                question.selection = question.selection.saturating_sub(1);
                            }
                        }
                        KeyCode::Down | KeyCode::Right if selection_count > 0 => {
                            if let Some(question) = self.pending_runtime_question.as_mut() {
                                question.selection =
                                    (question.selection + 1).min(selection_count.saturating_sub(1));
                            }
                        }
                        KeyCode::Char(' ')
                            if !editing_custom
                                && self.pending_runtime_question.as_ref().is_some_and(|q| {
                                    q.mode == heycode_core::QuestionMode::MultipleChoice
                                }) =>
                        {
                            if let Some(question) = self.pending_runtime_question.as_mut()
                                && !question.selected_choices.insert(question.selection)
                            {
                                question.selected_choices.remove(&question.selection);
                            }
                        }
                        KeyCode::Enter => self.resolve_runtime_question(),
                        KeyCode::Esc => self.cancel_runtime_question(),
                        KeyCode::Backspace if editing_custom => {
                            if let Some(question) = self.pending_runtime_question.as_mut() {
                                question.input.pop();
                            }
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            if let Some(question) = self.pending_runtime_question.as_mut()
                                && question.input.len() < 16 * 1024
                                && !character.is_control()
                            {
                                // Typing is an unambiguous request to use the
                                // custom answer, even while a suggested choice
                                // is highlighted.
                                question.selection = question.choices.len();
                                question.input.push(character);
                            }
                        }
                        _ => {}
                    }
                    return false;
                }
                Event::Paste(text) => {
                    if let Some(question) = self.pending_runtime_question.as_mut() {
                        question.selection = question.choices.len();
                        let remaining = (16_usize * 1024).saturating_sub(question.input.len());
                        question.input.extend(
                            text.chars()
                                .filter(|character| !character.is_control())
                                .take(remaining),
                        );
                    }
                    return false;
                }
                _ => return false,
            }
        }
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press && self.pending_ask.is_some() => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.interrupt_approval_owner();
                    return false;
                }
                let choices = self.approval_choices().len();
                let remember = choices == ASK_CHOICES.len();
                let grant_edits = self.approval_grants_edits();
                let Some(ask) = self.pending_ask.as_mut() else {
                    return false; // guard guarantees presence
                };
                // While the reason editor is open it owns every key: a typed
                // `y` is a letter of a sentence, not an answer.
                if let Some(reason) = ask.reason.as_mut() {
                    match key.code {
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            reason.push(character);
                        }
                        KeyCode::Backspace => {
                            reason.pop();
                        }
                        KeyCode::Esc => ask.reason = None,
                        KeyCode::Enter => {
                            let answer = heycode_agent::AskAnswer::deny_with_reason(reason.clone());
                            self.resolve_ask_with(answer);
                        }
                        _ => {}
                    }
                    return false;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Left => {
                        ask.selection = ask.selection.checked_sub(1).unwrap_or(choices - 1);
                    }
                    KeyCode::Down | KeyCode::Right => {
                        ask.selection = (ask.selection + 1) % choices;
                    }
                    KeyCode::Esc => self.interrupt_approval_owner(),
                    // The card numbers its choices like the reference, so the
                    // digits have to answer it.
                    KeyCode::Char(digit)
                        if digit.to_digit(10).is_some_and(|value| {
                            value >= 1 && usize::try_from(value).is_ok_and(|value| value <= choices)
                        }) =>
                    {
                        let index = digit.to_digit(10).unwrap_or(1) as usize - 1;
                        if index == 1 && grant_edits {
                            self.accept_future_edits();
                            return false;
                        }
                        let answer = match index {
                            0 => heycode_agent::AskAnswer::Allow,
                            1 if remember => heycode_agent::AskAnswer::AllowSession,
                            _ => heycode_agent::AskAnswer::Deny,
                        };
                        self.resolve_ask_with(answer);
                    }
                    KeyCode::Char('n') => self.resolve_ask_with(heycode_agent::AskAnswer::Deny),
                    KeyCode::Char('y') => self.resolve_ask_with(heycode_agent::AskAnswer::Allow),
                    KeyCode::Char('a') if remember => {
                        if grant_edits {
                            self.accept_future_edits();
                        } else {
                            self.resolve_ask_with(heycode_agent::AskAnswer::AllowSession);
                        }
                    }
                    KeyCode::Char('r') | KeyCode::Tab => ask.reason = Some(String::new()),
                    KeyCode::Enter => {
                        if ask.selection == 1 && grant_edits {
                            self.accept_future_edits();
                            return false;
                        }
                        let answer = match ask.selection {
                            0 => heycode_agent::AskAnswer::Allow,
                            1 if remember => heycode_agent::AskAnswer::AllowSession,
                            _ => heycode_agent::AskAnswer::Deny,
                        };
                        self.resolve_ask_with(answer);
                    }
                    _ => {}
                }
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && self.pending_command_confirmation.is_some() =>
            {
                match key.code {
                    KeyCode::Left | KeyCode::Up => {
                        if let Some(confirm) = self.pending_command_confirmation.as_mut() {
                            confirm.selection = 0;
                        }
                    }
                    KeyCode::Right | KeyCode::Down => {
                        if let Some(confirm) = self.pending_command_confirmation.as_mut() {
                            confirm.selection = 1;
                        }
                    }
                    KeyCode::Esc => self.resolve_command_confirmation(false),
                    KeyCode::Enter => {
                        let interrupt = self
                            .pending_command_confirmation
                            .as_ref()
                            .is_some_and(|confirm| confirm.selection == 1);
                        self.resolve_command_confirmation(interrupt);
                    }
                    _ => {}
                }
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.theme_picker.is_some() => {
                self.handle_theme_picker_key(key.code);
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && self.scroll_speed_picker.is_some() =>
            {
                self.handle_scroll_speed_picker_key(key.code);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.keymap_picker.is_some() => {
                self.handle_keymap_picker_key(key.code);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.profile_picker.is_some() => {
                self.handle_profile_picker_key(key.code);
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && self.session_browser.is_some() =>
            {
                self.handle_session_browser_key(key.code, key.modifiers);
                return false;
            }
            Event::Key(key) if self.sandbox_panel.is_some() => {
                if key.kind == KeyEventKind::Press {
                    let (close, selection) =
                        self.sandbox_panel.as_mut().map_or((false, None), |panel| {
                            let close = panel.handle_key(key.code, key.modifiers);
                            (close, panel.take_selection())
                        });
                    if let Some(mode) = selection {
                        self.pending_sandbox_selection = Some(mode);
                        self.sandbox_panel = None;
                    } else if close {
                        self.sandbox_panel = None;
                        self.push_command_receipt("Sandbox unchanged".to_owned());
                    }
                }
                return false;
            }
            Event::Key(key) if self.autocompact_panel.is_some() => {
                if key.kind == KeyEventKind::Press
                    && let Some(receipt) = self
                        .autocompact_panel
                        .as_mut()
                        .and_then(|panel| panel.handle_key(key.code, key.modifiers))
                {
                    self.autocompact_panel = None;
                    self.push_command_receipt(receipt);
                }
                return false;
            }
            Event::Paste(_) | Event::Mouse(_)
                if self.sandbox_panel.is_some() || self.autocompact_panel.is_some() =>
            {
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && self.permission_picker.is_some() =>
            {
                self.handle_permission_picker_key(key.code);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.route_picker.is_some() => {
                self.handle_route_picker_key(key.code, key.modifiers);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.skills_panel.is_some() => {
                if let Some(panel) = self.skills_panel.as_mut()
                    && panel.handle_key(key.code, key.modifiers)
                        == crate::skills_panel::SkillsPanelAction::Close
                {
                    let receipt = panel.close_receipt();
                    self.close_capability_catalog();
                    self.push_command_receipt(receipt);
                }
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && self.capability_catalog.is_some() =>
            {
                self.handle_capability_catalog_key(key.code);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.settings_panel.is_some() => {
                self.handle_settings_panel_key(key.code, key.modifiers);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.mcp_panel.is_some() => {
                self.handle_mcp_panel_key(key.code, key.modifiers);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.plugin_panel.is_some() => {
                self.handle_plugin_panel_key(key.code, key.modifiers);
                return false;
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && self.command_palette.is_some() =>
            {
                self.handle_palette_key(key.code, key.modifiers);
                return false;
            }
            Event::Mouse(_) if self.effort_picker.is_some() => return false,
            Event::Paste(_) if self.effort_picker.is_some() => return false,
            Event::Key(key) if key.kind == KeyEventKind::Press && self.effort_picker.is_some() => {
                self.handle_effort_picker_key(key.code);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.model_picker.is_some() => {
                self.handle_model_picker_key(key.code, key.modifiers);
                return false;
            }
            Event::Paste(text) if self.model_picker.is_some() => {
                if let Some(picker) = self.model_picker.as_mut() {
                    let remaining = 128_usize.saturating_sub(picker.query.chars().count());
                    picker.query.extend(text.chars().take(remaining));
                }
                self.refresh_model_matches();
                return false;
            }
            Event::Paste(text) if self.route_picker.is_some() => {
                if let Some(picker) = self.route_picker.as_mut() {
                    let remaining = 128_usize.saturating_sub(picker.query.chars().count());
                    picker.query.extend(text.chars().take(remaining));
                }
                self.refresh_route_matches();
                return false;
            }
            Event::Paste(text) if self.settings_panel.is_some() => {
                if let Some(panel) = self.settings_panel.as_mut() {
                    panel.paste(text);
                }
                return false;
            }
            Event::Paste(text) if self.command_palette.is_some() => {
                // Pasted text is composer text like any other; the palette
                // follows the first token.
                let _ = self
                    .input
                    .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.navigated = false;
                }
                self.refresh_command_palette();
                return false;
            }
            // Bracketed paste: the block lands in the composer as one draft.
            // Nothing is sent — pasting a script must not run its first line.
            Event::Paste(_) if self.detailed_transcript => return false,
            Event::Paste(text) => {
                let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
                let _ = self.input.insert_str(normalised);
                return false;
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                use heycode_ui::keymap::KeymapAction;
                if self.handle_detailed_transcript_key(*key) {
                    return false;
                }
                if (key.code == KeyCode::BackTab
                    || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT)))
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    self.cycle_approval_mode();
                    return false;
                }
                // A standalone `?` is a request to see the shortcut list, not
                // a character: it replaces the idle footer and is never
                // inserted. Inside a draft it stays ordinary text, which is
                // why the empty-composer test comes first. Backspace and
                // Escape put the footer back.
                if self.shortcut_list_open && matches!(key.code, KeyCode::Backspace | KeyCode::Esc)
                {
                    self.shortcut_list_open = false;
                    return false;
                }
                if key.code == KeyCode::Char('?')
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !(self.vim_enabled && !self.vim_insert)
                    && self.input.lines().iter().all(String::is_empty)
                {
                    self.shortcut_list_open = !self.shortcut_list_open;
                    return false;
                }
                self.shortcut_list_open = false;
                // `/` at an empty composer is position-sensitive, not a bound
                // action: it must stay typable everywhere else, so it is
                // decided before the keymap rather than inside it.
                if key.code == KeyCode::Char('/')
                    && key.modifiers.is_empty()
                    && self.input.lines().iter().all(String::is_empty)
                {
                    let _ = self.input.insert_str("/");
                    self.open_command_palette();
                    return false;
                }
                // The composer mode is the same one the border and the
                // screen-reader projection show, turn or no turn: a badge that
                // reads VIM NORMAL while the router is in insert mode makes the
                // same keystroke mean two things. Esc during a live turn is the
                // one exception, because it is the documented interrupt.
                if self.vim_enabled {
                    if self.vim_insert && key.code == KeyCode::Esc && !self.active_turn {
                        self.vim_insert = false;
                        return false;
                    }
                    if !self.vim_insert && self.handle_vim_normal_key(key.code, key.modifiers) {
                        return false;
                    }
                }
                // Normal mode is not a text field: a key vim did not claim is
                // still routed to its bound action, and only the composer
                // insertion at the tail is suppressed.
                if key.modifiers.contains(KeyModifiers::ALT)
                    && matches!(key.code, KeyCode::Up | KeyCode::Down)
                {
                    let indices: Vec<_> = self
                        .items
                        .iter()
                        .enumerate()
                        .filter_map(|(index, item)| {
                            ((matches!(
                                item,
                                Item::Reasoning { .. }
                                    | Item::FindingsReport { .. }
                                    | Item::Compaction { .. }
                            ) || matches!(item, Item::Tool { view, .. } if !view.merged))
                                && item.group_parent().is_none()
                                && !item.is_quiet_running_tool())
                            .then_some(index)
                        })
                        .collect();
                    let focused = if key.code == KeyCode::Up {
                        indices.iter().rev().copied().find(|index| {
                            self.reasoning_focus.is_none_or(|current| *index < current)
                        })
                    } else {
                        indices.iter().copied().find(|index| {
                            self.reasoning_focus.is_none_or(|current| *index > current)
                        })
                    }
                    .or(self.reasoning_focus);
                    self.focus_reasoning_item(focused);
                    return false;
                }
                if self.reasoning_focus.is_some() && key.modifiers.is_empty() {
                    if key.code == KeyCode::Char('w')
                        && let Some(Item::Tool {
                            name,
                            result: Some((true, value)),
                            ..
                        }) = self.reasoning_focus.and_then(|index| self.items.get(index))
                        && name.strip_prefix("mcp__heycode__").unwrap_or(name) == "SendUserFile"
                    {
                        match crate::file_delivery::receipt(value) {
                            Some(receipt) => self.pending_delivery_save = Some(receipt.files),
                            None => self.items.push(Item::Error(
                                "No admitted local file receipt is available to save".to_owned(),
                            )),
                        }
                        return false;
                    }
                    if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        if let Some(index) = self.reasoning_focus {
                            self.toggle_reasoning_item(index);
                        }
                        return false;
                    }
                    if key.code == KeyCode::Esc {
                        self.focus_reasoning_item(None);
                        return false;
                    }
                    // Ordinary typing returns focus to the composer.
                    if matches!(key.code, KeyCode::Char(_)) {
                        self.focus_reasoning_item(None);
                    }
                }
                if key.code == KeyCode::Up
                    && key.modifiers.is_empty()
                    && self.recall_pending_messages()
                {
                    return false;
                }
                let vim_normal = self.vim_enabled && !self.vim_insert;
                let action =
                    crate::terminal::chord(*key).and_then(|chord| self.keymap.action(chord));
                match action {
                    // Esc with a side panel open closes the panel; interrupting
                    // the turn is the meaning only when nothing else is on top.
                    Some(KeymapAction::Interrupt) if self.side_panel.is_some() => {
                        self.side_panel = None;
                    }
                    Some(KeymapAction::Interrupt)
                        if self.active_turn || self.side_command_activity.is_some() =>
                    {
                        if let Some(f) = self.interrupt_fn.as_ref() {
                            f();
                            self.cancellation_requested = self.active_turn;
                        }
                        if self.side_command_activity.is_none() {
                            self.verb = None;
                        }
                        self.restore_interrupted_command_draft();
                    }
                    Some(KeymapAction::Quit) => {
                        // Claude Code semantics: a draft is cleared first; an
                        // empty composer arms a visible quit that the next
                        // Ctrl+C confirms and any other key disarms.
                        if self.active_turn || self.side_command_activity.is_some() {
                            if let Some(f) = self.interrupt_fn.as_ref() {
                                f();
                                self.cancellation_requested = self.active_turn;
                            }
                            if self.side_command_activity.is_none() {
                                self.verb = None;
                            }
                            self.restore_interrupted_command_draft();
                            self.ctrl_c_seen = false;
                            return false;
                        }
                        if !self.input.lines().iter().all(String::is_empty) {
                            self.input = tui_textarea::TextArea::default();
                            self.history_cursor = None;
                            self.ctrl_c_seen = false;
                            return false;
                        }
                        if self.ctrl_c_seen {
                            return true;
                        }
                        self.ctrl_c_seen = true;
                        return false;
                    }
                    Some(KeymapAction::ToggleTranscript) => self.toggle_detailed_transcript(),
                    Some(KeymapAction::ToggleReasoning) => {
                        if let Some(index) = self.reasoning_focus.or_else(|| {
                            self.items
                                .iter()
                                .rposition(|item| matches!(item, Item::Reasoning { .. }))
                        }) {
                            self.toggle_reasoning_item(index);
                        } else {
                            self.show_reasoning = !self.show_reasoning;
                        }
                    }
                    Some(KeymapAction::CommandPalette) => self.open_command_palette(),
                    Some(KeymapAction::CycleSidePanel) => self.cycle_side_panel(),
                    Some(KeymapAction::ToggleTasks) => self.open_tasks(),
                    Some(KeymapAction::ToggleWorkflows) => self.open_workflows(),
                    Some(KeymapAction::ScrollHalfPageUp) => {
                        if key.code == KeyCode::Char('u')
                            && key.modifiers == crossterm::event::KeyModifiers::CONTROL
                            && !vim_normal
                            && self.input.cursor().1 > 0
                        {
                            self.input.delete_line_by_head();
                            self.history_cursor = None;
                        } else {
                            self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(20);
                        }
                    }
                    Some(KeymapAction::ScrollHalfPageDown) => {
                        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(20);
                    }
                    Some(KeymapAction::ScrollPageUp) => {
                        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(15);
                    }
                    Some(KeymapAction::ScrollPageDown) => {
                        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(15);
                    }
                    Some(KeymapAction::ScrollToOldest) => {
                        self.scroll_from_bottom = usize::MAX;
                    }
                    Some(KeymapAction::ScrollToNewest) => self.scroll_from_bottom = 0,
                    Some(KeymapAction::InsertNewline) => {
                        if !vim_normal {
                            self.input.insert_newline();
                        }
                    }
                    // Shift+Enter is reported as a distinct chord only by
                    // terminals with the keyboard-enhancement protocol; when it
                    // is, it inserts a line break like Alt+Enter.
                    Some(KeymapAction::Submit)
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::SHIFT) =>
                    {
                        if !vim_normal {
                            self.input.insert_newline();
                        }
                    }
                    // Claude Code convention: a line ending in `\` continues on
                    // the next line instead of sending.
                    Some(KeymapAction::Submit) if self.current_line_ends_with_backslash() => {
                        self.input.delete_char();
                        self.input.insert_newline();
                    }
                    Some(KeymapAction::Submit) => self.submit_composer(),
                    Some(KeymapAction::QueueFollowUp) if self.active_turn => {
                        self.queue_follow_up();
                    }
                    Some(KeymapAction::QueueFollowUp) => {
                        if !vim_normal {
                            let key_event =
                                crossterm::event::KeyEvent::new(key.code, key.modifiers);
                            let _ = self.input.input(tui_textarea::Input::from(key_event));
                        }
                    }
                    // Interrupt while idle, and every unbound key, is composer
                    // input. A key the user rebound away therefore types, which
                    // is what makes a rebinding complete rather than additive.
                    Some(KeymapAction::Interrupt) | None => {
                        if !vim_normal && !self.navigate_prompt_history(key.code) {
                            let key_event =
                                crossterm::event::KeyEvent::new(key.code, key.modifiers);
                            let _ = self.input.input(tui_textarea::Input::from(key_event));
                            if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
                                self.history_cursor = None;
                            }
                        }
                    }
                }
                // Any key that was not the quit chord disarms a pending quit.
                if action != Some(KeymapAction::Quit) {
                    self.ctrl_c_seen = false;
                }
            }
            _ => {}
        }
        false
    }
}

impl AppState {
    /// Hint to show while a quit is armed by a first Ctrl+C on an empty composer.
    #[must_use]
    pub fn quit_hint(&self) -> Option<&'static str> {
        self.ctrl_c_seen.then_some("press Ctrl+C again to exit")
    }

    /// Whether the shortcut list stands in place of the idle footer line.
    ///
    /// A draft the user has started wins: the list is a discovery aid for an
    /// empty composer. The detailed transcript has its own read-only controls
    /// and keeps any draft hidden, so its help is available independently.
    #[must_use]
    pub fn shortcut_list_visible(&self) -> bool {
        self.shortcut_list_open
            && (self.detailed_transcript || self.input.lines().iter().all(String::is_empty))
    }

    /// The shortcut rows resolved against this session's live bindings.
    #[must_use]
    pub(crate) fn shortcut_list(&self) -> crate::composer_shortcuts::ShortcutColumns {
        crate::composer_shortcuts::ShortcutColumns::resolve(&self.keymap)
    }

    fn current_line_ends_with_backslash(&self) -> bool {
        let (row, col) = self.input.cursor();
        self.input
            .lines()
            .get(row)
            .is_some_and(|line| col == line.chars().count() && line.ends_with('\\'))
    }

    /// Up/Down on the first/last line walk sent prompts, oldest last; the
    /// draft the user was typing comes back after the newest entry.
    /// Returns `true` when the key was consumed.
    fn navigate_prompt_history(&mut self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode;
        if self.prompt_history.is_empty() {
            return false;
        }
        let (row, _) = self.input.cursor();
        let last_row = self.input.lines().len().saturating_sub(1);
        match code {
            KeyCode::Up if row == 0 => {
                let next = match &self.history_cursor {
                    None => self.prompt_history.len() - 1,
                    Some((index, _)) => index.saturating_sub(1),
                };
                let draft = match self.history_cursor.take() {
                    Some((_, draft)) => draft,
                    None => self.input.lines().to_vec(),
                };
                self.replace_input(self.prompt_history[next].clone());
                self.history_cursor = Some((next, draft));
                true
            }
            KeyCode::Down if row == last_row => {
                let Some((index, draft)) = self.history_cursor.take() else {
                    return false;
                };
                if index + 1 < self.prompt_history.len() {
                    self.replace_input(self.prompt_history[index + 1].clone());
                    self.history_cursor = Some((index + 1, draft));
                } else {
                    self.replace_input(draft.join("\n"));
                }
                true
            }
            _ => false,
        }
    }

    fn record_prompt_history(&mut self, text: &str) {
        if text.trim().is_empty() || self.prompt_history.last().is_some_and(|last| last == text) {
            return;
        }
        self.prompt_history.push(text.to_owned());
        if self.prompt_history.len() > crate::prompt_history::MAX_PROMPT_HISTORY {
            self.prompt_history.remove(0);
        }
        self.history_cursor = None;
        if let Some(store) = self.prompt_history_store.as_ref() {
            store.append(text);
        }
    }

    /// Recall this home's prompts and keep new ones, so Up reaches yesterday.
    ///
    /// Seeding replaces whatever this session had recalled; it is a startup
    /// step, before anything is typed.
    pub fn set_prompt_history_store(
        &mut self,
        store: Arc<crate::prompt_history::PromptHistoryStore>,
    ) {
        self.prompt_history = store.load();
        self.history_cursor = None;
        self.prompt_history_store = Some(store);
    }
}

impl AppState {
    /// Apply one vim normal-mode motion; `true` when the key was consumed.
    ///
    /// A key this does not claim — every Ctrl/Alt chord and every unmapped
    /// code — is left for the keymap, so a bound action never dies just
    /// because the composer is idle in normal mode.
    fn handle_vim_normal_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        let translated = match code {
            KeyCode::Char('i') => {
                self.vim_insert = true;
                return true;
            }
            KeyCode::Char('a') => {
                self.vim_insert = true;
                Some(KeyCode::Right)
            }
            KeyCode::Char('h') => Some(KeyCode::Left),
            KeyCode::Char('j') => Some(KeyCode::Down),
            KeyCode::Char('k') => Some(KeyCode::Up),
            KeyCode::Char('l') => Some(KeyCode::Right),
            KeyCode::Char('0') => Some(KeyCode::Home),
            KeyCode::Char('$') => Some(KeyCode::End),
            KeyCode::Char('x') => Some(KeyCode::Delete),
            KeyCode::Enter => {
                self.submit_composer();
                return true;
            }
            // Everything else, Esc included, belongs to the keymap: Esc
            // interrupts a live turn and is a no-op while idle.
            _ => None,
        };
        let Some(code) = translated else {
            return false;
        };
        let _ = self.input.input(tui_textarea::Input::from(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )));
        true
    }

    /// Whether the standard immediate approval-mode command is available.
    pub fn can_cycle_approval_mode(&self) -> bool {
        self.command_registry.as_ref().is_some_and(|commands| {
            matches!(commands.get("permissions"), Ok(Some(command))
                if command.availability().is_available()
                    && command.descriptor().timing() == heycode_agent::CommandTiming::Immediate)
        })
    }

    fn cycle_approval_mode(&mut self) {
        if self.pending_send.is_some() || !self.can_cycle_approval_mode() {
            return;
        }
        let next = match self.permission.as_str() {
            "full_access" => "accepted_edits",
            "accepted_edits" => "ask",
            "ask" | "default" if self.runtime == "native" => "plan",
            "plan" => "full_access",
            _ => "full_access",
        };
        // Use the same policy owner as /permissions. Its committed result
        // refreshes the footer; retain the draft and never claim an early switch.
        self.quiet_permission_mode = Some(next.to_owned());
        self.pending_send = Some(format!("/permissions {next}"));
    }

    fn submit_composer(&mut self) {
        if self.workspace_trust.is_some() {
            return;
        }
        let text = self.input.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        // Submitting is an explicit request to see the new result. Resume the
        // live edge even when the user previously scrolled through a long
        // command response (Ctrl+U is the default half-page-up binding).
        self.scroll_from_bottom = 0;
        if !self.active_turn {
            // A slash line is a command, never model input: an unknown or
            // unavailable command is reported here and the text is kept so
            // the user can correct it.
            if let Some((name, args)) = parse_slash(&text)
                && let Some(commands) = self.command_registry.as_ref()
            {
                match commands.get(&name) {
                    Ok(Some(command)) if !command.availability().is_available() => {
                        self.items.push(Item::Error(format!(
                            "/{name} unavailable: {}",
                            command
                                .availability()
                                .reason()
                                .unwrap_or("unknown prerequisite")
                        )));
                        self.input = tui_textarea::TextArea::default();
                        return;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        self.items.push(Item::Notice(unknown_command_notice(&name)));
                        self.input = tui_textarea::TextArea::default();
                        return;
                    }
                    Err(error) => {
                        self.items.push(Item::Error(error.to_string()));
                        return;
                    }
                }
                self.record_accepted_command(&name, &args);
            }
            if parse_slash(&text).is_none() {
                self.record_prompt_history(&text);
            }
            self.pending_send = Some(text);
            self.submitted_command_draft = self
                .pending_send
                .as_deref()
                .filter(|text| {
                    parse_slash(text).is_some_and(|(name, args)| {
                        name == "compact" || (name == "recap" && args.trim().is_empty())
                    })
                })
                .map(|_| self.input.clone());
            self.input = tui_textarea::TextArea::default();
            return;
        }
        let Some((name, args)) = parse_slash(&text) else {
            if !self.native_inbox_available() {
                self.items.push(Item::Info(
                    "steering is unavailable for this delegated runtime in the current TUI"
                        .to_owned(),
                ));
                return;
            }
            self.queue_inbox_submission(heycode_session::InboxDelivery::Steer, text);
            return;
        };
        let Some(commands) = self.command_registry.as_ref() else {
            self.items
                .push(Item::Error("command registry is not attached".to_owned()));
            return;
        };
        let command = match commands.get(&name) {
            Ok(Some(command)) => command,
            Ok(None) => {
                self.items.push(Item::Notice(unknown_command_notice(&name)));
                return;
            }
            Err(error) => {
                self.items.push(Item::Error(error.to_string()));
                return;
            }
        };
        let availability = command.availability();
        if !availability.is_available() {
            self.items.push(Item::Error(format!(
                "/{name} unavailable: {}",
                availability.reason().unwrap_or("unknown prerequisite")
            )));
            return;
        }
        match route_command(command.descriptor().timing(), true) {
            CommandDisposition::ExecuteNow => {
                self.record_accepted_command(&name, &args);
                if name == "recap" && args.trim().is_empty() {
                    self.submitted_command_draft = Some(self.input.clone());
                }
                self.pending_send = Some(text);
                self.input = tui_textarea::TextArea::default();
            }
            CommandDisposition::Queue => {
                self.record_accepted_command(&name, &args);
                self.queue_command(text, command.descriptor().synopsis());
                self.input = tui_textarea::TextArea::default();
            }
            CommandDisposition::ConfirmInterrupt => {
                self.request_command_confirmation(text, command.descriptor().synopsis());
                self.input = tui_textarea::TextArea::default();
            }
        }
    }

    fn resolve_command_confirmation(&mut self, interrupt: bool) {
        let Some(confirm) = self.pending_command_confirmation.take() else {
            return;
        };
        if !interrupt {
            self.replace_input(confirm.text);
            return;
        }
        let Some(interrupt_fn) = self.interrupt_fn.as_ref() else {
            self.items.push(Item::Error(
                "active operation has no interrupt handle".to_owned(),
            ));
            self.replace_input(confirm.text);
            return;
        };
        interrupt_fn();
        self.cancellation_requested = self.active_turn;
        if let Some((name, args)) = parse_slash(&confirm.text) {
            self.record_accepted_command(&name, &args);
        }
        self.items.push(Item::Info(format!(
            "interrupting active work; queued {}",
            confirm.synopsis
        )));
        self.queued_commands.push_front(QueuedCommand {
            text: confirm.text,
            synopsis: confirm.synopsis,
        });
        self.verb = None;
    }

    fn replace_input(&mut self, text: String) {
        let mut input = tui_textarea::TextArea::default();
        let _ = input.insert_str(text);
        self.input = input;
    }

    fn record_accepted_command(&mut self, name: &str, args: &str) {
        self.items
            .push(Item::Command(command_transcript_label(name, args)));
    }

    fn refresh_model_matches(&mut self) {
        let Some(picker) = self.model_picker.as_ref() else {
            return;
        };
        let ModelPickerLoadState::Ready { snapshot, .. } = &picker.state else {
            return;
        };
        let snapshot = snapshot.clone();
        let filter = picker.filter;
        let query = picker.query.clone();
        let current_model = picker.current_model.clone();
        let mut matches = filter_models(&snapshot, filter, &query, unix_time_ms());
        let unmatched_overrides =
            self.catalog_overrides
                .as_ref()
                .map_or_else(Vec::new, |overrides| {
                    let attributed = overrides.attribute(&snapshot);
                    for matched in &mut matches {
                        if let Some(model) = attributed.model(&matched.model.id) {
                            let assertions = model.assertions();
                            matched.has_contradiction = assertions.iter().any(|assertion| {
                                assertion.direction()
                                    == heycode_catalog_file::AssertionDirection::ContradictsEvidence
                            });
                            matched.assertions = assertions
                                .into_iter()
                                .map(|assertion| assertion.describe())
                                .collect();
                        }
                    }
                    attributed
                        .unmatched()
                        .iter()
                        .map(heycode_catalog_file::UnmatchedOverride::describe)
                        .collect()
                });
        let selected = if query.is_empty() {
            matches
                .iter()
                .position(|row| row.model.id == current_model)
                .unwrap_or(0)
        } else {
            0
        };
        if let Some(picker) = self.model_picker.as_mut() {
            picker.matches = matches;
            picker.selected = selected;
            picker.unmatched_overrides = unmatched_overrides;
        }
    }

    fn handle_model_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match code {
            KeyCode::Esc => {
                // Dismissing the picker still reports the route in force,
                // which is what the pinned `/model` + Escape capture shows.
                let kept = format!("Kept model as {}", self.active_model_label());
                self.close_model_picker();
                self.push_command_receipt(kept);
            }
            KeyCode::Up => {
                if let Some(picker) = self.model_picker.as_mut()
                    && !picker.matches.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.matches.len() - 1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = self.model_picker.as_mut()
                    && !picker.matches.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.matches.len();
                }
            }
            KeyCode::Tab => {
                if let Some(picker) = self.model_picker.as_mut() {
                    picker.filter = picker.filter.next();
                }
                self.refresh_model_matches();
            }
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(picker) = self.model_picker.as_mut() {
                    picker.state = ModelPickerLoadState::Loading;
                    picker.matches.clear();
                    picker.selected = 0;
                    picker.unmatched_overrides.clear();
                    picker.effort_model = None;
                    self.model_refresh_request = Some(heycode_llm::CatalogRefreshMode::Force);
                }
            }
            KeyCode::Char('/')
                if self
                    .model_picker
                    .as_ref()
                    .is_some_and(|picker| !picker.search_active) =>
            {
                if let Some(picker) = self.model_picker.as_mut() {
                    picker.search_active = true;
                }
            }
            KeyCode::Backspace => {
                if let Some(picker) = self.model_picker.as_mut() {
                    picker.query.pop();
                }
                self.refresh_model_matches();
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some(picker) = self.model_picker.as_mut()
                    && let Some(effort) = picker.effort_picker.as_mut()
                    && !effort.choices.is_empty()
                    && picker
                        .matches
                        .get(picker.selected)
                        .is_some_and(|row| Some(&row.model.id) == picker.effort_model.as_ref())
                {
                    effort.selected = if matches!(code, KeyCode::Left) {
                        effort.selected.saturating_sub(1)
                    } else {
                        effort
                            .selected
                            .saturating_add(1)
                            .min(effort.choices.len() - 1)
                    };
                }
            }
            KeyCode::Enter | KeyCode::Char('s')
                if modifiers.is_empty()
                    && (matches!(code, KeyCode::Enter)
                        || self
                            .model_picker
                            .as_ref()
                            .is_some_and(|picker| !picker.search_active)) =>
            {
                let selected = self.model_picker.as_ref().and_then(|picker| {
                    let ModelPickerLoadState::Ready { snapshot, .. } = &picker.state else {
                        return None;
                    };
                    picker
                        .matches
                        .get(picker.selected)
                        .map(|row| ModelPickerSelection {
                            owner: picker.owner.clone(),
                            revision: picker.routing_revision,
                            catalog: snapshot.clone(),
                            model: row.model.id.clone(),
                            scope: if matches!(code, KeyCode::Char('s')) {
                                heycode_routing::SelectionScope::Session
                            } else {
                                heycode_routing::SelectionScope::Default
                            },
                            effort: (picker.effort_model.as_ref() == Some(&row.model.id))
                                .then(|| {
                                    picker.effort_picker.as_ref().and_then(|effort| {
                                        effort.choices.get(effort.selected).cloned()
                                    })
                                })
                                .flatten(),
                        })
                });
                if let Some(selection) = selected {
                    self.pending_model_selection = Some(selection);
                    self.close_model_picker();
                    self.close_effort_picker();
                }
            }
            KeyCode::Char(character)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(picker) = self.model_picker.as_mut()
                    && picker.query.chars().count() < 128
                {
                    picker.search_active = true;
                    picker.query.push(character);
                }
                self.refresh_model_matches();
            }
            _ => {}
        }
    }

    fn handle_effort_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => {
                self.close_effort_picker();
                self.push_command_receipt(CANCELLED_EFFORT_SELECTION);
            }
            KeyCode::Up | KeyCode::Left => {
                if let Some(picker) = self.effort_picker.as_mut()
                    && !picker.choices.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.choices.len() - 1);
                }
            }
            KeyCode::Down | KeyCode::Right => {
                if let Some(picker) = self.effort_picker.as_mut()
                    && !picker.choices.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.choices.len();
                }
            }
            KeyCode::Enter | KeyCode::Char('s') => {
                let selection = self.effort_picker.as_ref().and_then(|picker| {
                    picker.choices.get(picker.selected).map(|effort| {
                        (
                            picker.owner.clone(),
                            picker.routing_revision,
                            effort.clone(),
                            if code == KeyCode::Char('s') {
                                heycode_routing::SelectionScope::Session
                            } else {
                                heycode_routing::SelectionScope::Default
                            },
                        )
                    })
                });
                if let Some(selection) = selection {
                    self.pending_effort_selection = Some(selection);
                    self.close_effort_picker();
                }
            }
            _ => {}
        }
    }

    fn handle_profile_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => self.profile_picker = None,
            KeyCode::Up => {
                if let Some(picker) = self.profile_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.rows.len() - 1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = self.profile_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.rows.len();
                }
            }
            KeyCode::Enter => {
                let selected = self.profile_picker.as_ref().and_then(|picker| {
                    picker.rows.get(picker.selected).map(|row| row.name.clone())
                });
                if let Some(row) = selected {
                    self.select_profile(row);
                }
            }
            _ => {}
        }
    }

    fn handle_permission_picker_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Esc => self.close_permission_picker(),
            KeyCode::Up => {
                if let Some(picker) = self.permission_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.rows.len() - 1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = self.permission_picker.as_mut()
                    && !picker.rows.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.rows.len();
                }
            }
            KeyCode::Enter => {
                let selected = self
                    .permission_picker
                    .as_ref()
                    .and_then(|picker| picker.rows.get(picker.selected))
                    .cloned();
                if let Some(row) = selected
                    && row.selectable
                    && self.pending_send.is_none()
                {
                    if !row.current {
                        let mode = row.mode.as_str();
                        self.quiet_permission_mode = Some(mode.to_owned());
                        self.pending_send = Some(format!("/permissions {mode}"));
                    }
                    self.close_permission_picker();
                }
            }
            _ => {}
        }
    }

    fn refresh_route_matches(&mut self) {
        let Some(picker) = self.route_picker.as_ref() else {
            return;
        };
        let matches = filter_routes(&picker.rows, picker.filter, &picker.query);
        let selected = if picker.query.is_empty() {
            matches
                .iter()
                .position(|row| row.row.current && row.row.selection.is_some())
                .unwrap_or(0)
        } else {
            0
        };
        if let Some(picker) = self.route_picker.as_mut() {
            picker.matches = matches;
            picker.selected = selected;
        }
    }

    fn handle_route_picker_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        use crossterm::event::{KeyCode, KeyModifiers};
        match code {
            KeyCode::Esc => self.close_route_picker(),
            KeyCode::Up => {
                if let Some(picker) = self.route_picker.as_mut()
                    && !picker.matches.is_empty()
                {
                    picker.selected = picker
                        .selected
                        .checked_sub(1)
                        .unwrap_or(picker.matches.len() - 1);
                }
            }
            KeyCode::Down => {
                if let Some(picker) = self.route_picker.as_mut()
                    && !picker.matches.is_empty()
                {
                    picker.selected = (picker.selected + 1) % picker.matches.len();
                }
            }
            KeyCode::Tab => {
                if let Some(picker) = self.route_picker.as_mut() {
                    picker.filter = picker.filter.next();
                }
                self.refresh_route_matches();
            }
            KeyCode::Backspace => {
                if let Some(picker) = self.route_picker.as_mut() {
                    picker.query.pop();
                }
                self.refresh_route_matches();
            }
            KeyCode::Enter => {
                let selected = self.route_picker.as_ref().and_then(|picker| {
                    picker
                        .matches
                        .get(picker.selected)
                        .map(|row| row.row.clone())
                });
                if let Some(row) = selected {
                    match row.selection {
                        Some(selection) => {
                            self.pending_route_selection = Some(selection);
                            self.close_route_picker();
                        }
                        None => self.items.push(Item::Error(format!(
                            "{} unavailable: {}",
                            row.display_name,
                            row.unavailable_reason
                                .as_deref()
                                .unwrap_or("route cannot be activated")
                        ))),
                    }
                }
            }
            KeyCode::Char(character)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(picker) = self.route_picker.as_mut()
                    && picker.query.chars().count() < 128
                {
                    picker.query.push(character);
                }
                self.refresh_route_matches();
            }
            _ => {}
        }
    }

    fn close_route_picker(&mut self) {
        self.route_picker = None;
        self.route_picker_request = None;
    }

    fn close_mcp_panel(&mut self) {
        self.mcp_panel = None;
    }

    fn close_plugin_panel(&mut self) {
        self.plugin_panel = None;
    }

    pub(crate) fn sandbox_panel(&self) -> Option<&crate::sandbox_panel::SandboxPanel> {
        self.sandbox_panel.as_ref()
    }

    pub(crate) fn autocompact_panel(&self) -> Option<&crate::autocompact_panel::AutoCompactPanel> {
        self.autocompact_panel.as_ref()
    }

    fn close_permission_picker(&mut self) {
        self.permission_picker = None;
        self.sandbox_panel = None;
        self.autocompact_panel = None;
    }

    fn close_model_picker(&mut self) {
        if self.model_picker.take().is_some() {
            self.model_refresh_cancel_requested = true;
        }
    }

    fn close_effort_picker(&mut self) {
        self.effort_picker = None;
    }

    fn open_command_palette(&mut self) {
        self.close_capability_catalog();
        self.close_settings_panel();
        self.session_browser = None;
        if self.workspace_trust.is_some() {
            return;
        }
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        let Some(commands) = self.command_registry.clone() else {
            self.items
                .push(Item::Error("command registry is not attached".to_owned()));
            return;
        };
        // The palette is always a view over a slash composer. A non-slash
        // draft is set aside and restored if nothing runs.
        let restore_draft = if palette_query(&self.input).is_some() {
            None
        } else {
            let draft = std::mem::take(&mut self.input);
            self.replace_input("/".to_owned());
            Some(draft)
        };
        match commands.catalog() {
            Ok(catalog) => {
                let query = palette_query(&self.input).unwrap_or_default().to_owned();
                let matches = filter_commands(&catalog, &query);
                self.command_palette = Some(CommandPaletteView {
                    selected: first_preselectable(&matches),
                    query,
                    matches,
                    navigated: false,
                    restore_draft,
                });
            }
            Err(error) => self.items.push(Item::Error(error.to_string())),
        }
    }

    /// Recompute the palette from the composer after it changed. Closes the
    /// palette when the composer no longer holds a single slash line.
    fn refresh_command_palette(&mut self) {
        let Some(commands) = self.command_registry.as_ref() else {
            self.command_palette = None;
            return;
        };
        let Some(query) = palette_query(&self.input).map(str::to_owned) else {
            self.command_palette = None;
            return;
        };
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        match commands.catalog() {
            Ok(catalog) => {
                let previous = palette
                    .highlighted()
                    .map(|index| palette.matches[index].entry.descriptor.id().to_owned());
                palette.matches = filter_commands(&catalog, &query);
                palette.query = query;
                // A navigated highlight follows its row; otherwise the first
                // preselectable row (a real name match) is highlighted.
                palette.selected = match previous.filter(|_| palette.navigated) {
                    Some(id) => palette
                        .matches
                        .iter()
                        .position(|row| row.entry.descriptor.id() == id)
                        .unwrap_or_else(|| first_preselectable(&palette.matches)),
                    None => first_preselectable(&palette.matches),
                };
            }
            Err(error) => {
                self.command_palette = None;
                self.items.push(Item::Error(error.to_string()));
            }
        }
    }

    fn handle_palette_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        match code {
            KeyCode::Esc => self.close_command_palette(true),
            KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.close_command_palette(true);
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(palette) = self.command_palette.as_mut()
                    && !palette.matches.is_empty()
                {
                    let len = palette.matches.len();
                    palette.selected = match (code, palette.highlighted()) {
                        (KeyCode::Up, Some(index)) => index.checked_sub(1).unwrap_or(len - 1),
                        (KeyCode::Up, None) => len - 1,
                        (_, Some(index)) => (index + 1) % len,
                        (_, None) => 0,
                    };
                    palette.navigated = true;
                }
            }
            KeyCode::Enter => self.run_or_complete_palette_command(),
            KeyCode::Char('u')
                if modifiers == KeyModifiers::CONTROL
                    && crate::terminal::chord(KeyEvent::new(code, modifiers))
                        .and_then(|chord| self.keymap.action(chord))
                        == Some(heycode_ui::keymap::KeymapAction::ScrollHalfPageUp) =>
            {
                self.input.delete_line_by_head();
                self.history_cursor = None;
                self.refresh_command_palette();
            }
            // Everything else is composer input; the palette follows the text.
            _ => {
                let _ = self
                    .input
                    .input(tui_textarea::Input::from(KeyEvent::new(code, modifiers)));
                if let Some(palette) = self.command_palette.as_mut() {
                    palette.navigated = false;
                }
                self.refresh_command_palette();
            }
        }
    }

    /// Close the palette. `restore` puts back a draft displaced by Ctrl+P.
    fn close_command_palette(&mut self, restore: bool) {
        let Some(palette) = self.command_palette.take() else {
            return;
        };
        if restore && let Some(draft) = palette.restore_draft {
            self.input = draft;
        }
    }

    /// Enter in the palette. What the user typed wins: an exact command id or
    /// any typed arguments run as written; a prefix of a highlighted
    /// argument-free command completes and runs; a prefix of an argument-taking
    /// command completes into the composer for the user to finish; anything
    /// else is submitted so the composer reports the unknown command. A
    /// description-only match runs only after the user arrowed to it.
    fn run_or_complete_palette_command(&mut self) {
        let text = self.input.lines().join("\n");
        let Some((typed_id, typed_args)) = parse_slash(&text) else {
            self.close_command_palette(false);
            self.submit_composer();
            return;
        };
        let typed_exists = self
            .command_registry
            .as_ref()
            .is_some_and(|commands| matches!(commands.get(&typed_id), Ok(Some(_))));
        let highlighted = self.command_palette.as_ref().and_then(|palette| {
            palette
                .highlighted()
                .map(|index| &palette.matches[index])
                .filter(|row| row.preselectable || palette.navigated)
                .map(|row| {
                    (
                        row.entry.descriptor.id().to_owned(),
                        !row.entry.descriptor.arguments().is_empty(),
                        row.entry.availability.clone(),
                    )
                })
        });
        if typed_exists || !typed_args.is_empty() {
            let unavailable =
                self.command_registry
                    .as_ref()
                    .and_then(|commands| match commands.get(&typed_id) {
                        Ok(Some(command)) if !command.availability().is_available() => {
                            Some(command.availability())
                        }
                        _ => None,
                    });
            if let Some(availability) = unavailable {
                // Say why, then clear: leaving `/effort` in the composer with a
                // one-row palette only traps the next thing the user types.
                self.items.push(Item::Error(format!(
                    "/{typed_id} unavailable: {}",
                    availability.reason().unwrap_or("unknown prerequisite")
                )));
                self.input = tui_textarea::TextArea::default();
                self.close_command_palette(false);
                return;
            }
            let side_draft = if matches!(
                typed_id.as_str(),
                "btw"
                    | "voice"
                    | "add-dir"
                    | "help"
                    | "memory"
                    | "copy"
                    | "export"
                    | "skills"
                    | "settings"
                    | "config"
            ) {
                self.command_palette
                    .as_ref()
                    .and_then(|palette| palette.restore_draft.clone())
            } else {
                None
            };
            self.close_command_palette(false);
            self.submit_composer();
            if let Some(draft) = side_draft {
                self.input = draft;
            }
            return;
        }
        match highlighted {
            Some((id, _, availability)) if !availability.is_available() => {
                self.items.push(Item::Error(format!(
                    "/{id} unavailable: {}",
                    availability.reason().unwrap_or("unknown prerequisite")
                )));
                self.input = tui_textarea::TextArea::default();
                self.close_command_palette(false);
            }
            Some((id, false, _)) => {
                let side_draft = if matches!(
                    id.as_str(),
                    "help" | "memory" | "copy" | "export" | "skills" | "settings" | "config"
                ) {
                    self.command_palette
                        .as_ref()
                        .and_then(|palette| palette.restore_draft.clone())
                } else {
                    None
                };
                self.replace_input(format!("/{id}"));
                self.close_command_palette(false);
                self.submit_composer();
                if let Some(draft) = side_draft {
                    self.input = draft;
                }
            }
            Some((id, true, _)) => {
                self.replace_input(format!("/{id} "));
                if matches!(
                    id.as_str(),
                    "btw"
                        | "voice"
                        | "add-dir"
                        | "help"
                        | "memory"
                        | "copy"
                        | "export"
                        | "skills"
                        | "settings"
                        | "config"
                ) {
                    self.refresh_command_palette();
                } else {
                    self.close_command_palette(false);
                }
            }
            None => {
                self.close_command_palette(false);
                self.submit_composer();
            }
        }
    }

    fn submit_secret_prompt(&mut self) {
        let Some(mut view) = self.pending_secret.take() else {
            return;
        };
        if view.secret.is_empty() {
            self.pending_secret = Some(view);
            return;
        }
        if let Some(prompt) = self.secret_prompt.as_ref() {
            let secret =
                heycode_credentials::CredentialSecret::new(std::mem::take(&mut view.secret));
            let _ = prompt.answer(view.id, secret);
        }
    }

    fn cancel_secret_prompt(&mut self) {
        if let Some(view) = self.pending_secret.take()
            && let Some(prompt) = self.secret_prompt.as_ref()
        {
            let _ = prompt.cancel(view.id);
        }
    }

    fn apply_onboarding_action(&mut self, action: heycode_onboarding::OnboardingAction) {
        let Some(service) = self.onboarding_service.as_ref() else {
            return;
        };
        match service.apply(action) {
            Ok(outcome) => {
                if outcome == heycode_onboarding::OnboardingOutcome::Exit {
                    self.quit_requested = true;
                }
                if outcome != heycode_onboarding::OnboardingOutcome::None {
                    self.onboarding_outcome = Some(outcome);
                }
                match service.snapshot() {
                    Ok(snapshot) => self.onboarding = Some(snapshot),
                    Err(error) => self.items.push(Item::Error(error.to_string())),
                }
            }
            Err(error) => self.items.push(Item::Error(error.to_string())),
        }
    }

    fn handle_workspace_trust_event(&mut self, event: &crossterm::event::Event) -> bool {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.ctrl_c_seen {
                self.workspace_trust = None;
                self.run_outcome = Some(TuiRunOutcome::Exit);
                return true;
            }
            self.ctrl_c_seen = true;
            return false;
        }
        self.ctrl_c_seen = false;
        match key.code {
            KeyCode::Up | KeyCode::Left => {
                if let Some(view) = self.workspace_trust.as_mut() {
                    let len = view.prompt.state().actions().len();
                    if len > 0 {
                        view.selected = view.selected.checked_sub(1).unwrap_or(len - 1);
                    }
                    view.error = None;
                }
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                if let Some(view) = self.workspace_trust.as_mut() {
                    let len = view.prompt.state().actions().len();
                    if len > 0 {
                        view.selected = (view.selected + 1) % len;
                    }
                    view.error = None;
                }
            }
            KeyCode::Enter => {
                let action = self
                    .workspace_trust
                    .as_ref()
                    .and_then(|view| view.prompt.state().actions().get(view.selected).copied());
                if let Some(action) = action {
                    return self.apply_workspace_trust_action(action);
                }
            }
            KeyCode::Esc => {
                return self
                    .apply_workspace_trust_action(heycode_trust::WorkspaceTrustAction::Exit);
            }
            _ => {}
        }
        false
    }

    fn apply_workspace_trust_action(
        &mut self,
        action: heycode_trust::WorkspaceTrustAction,
    ) -> bool {
        let result = self
            .workspace_trust
            .as_ref()
            .map(|view| view.prompt.apply(action));
        match result {
            Some(Ok(heycode_trust::WorkspaceTrustActionOutcome::Ready(snapshot))) => {
                self.finish_workspace_trust(snapshot);
                false
            }
            Some(Ok(heycode_trust::WorkspaceTrustActionOutcome::Exit)) => {
                self.workspace_trust = None;
                self.run_outcome = Some(TuiRunOutcome::Exit);
                true
            }
            Some(Err(error)) => {
                let refresh = self
                    .workspace_trust
                    .as_mut()
                    .map(|view| view.prompt.refresh());
                match refresh {
                    Some(Ok(heycode_trust::TrustStartupState::Ready(snapshot))) => {
                        self.finish_workspace_trust(snapshot);
                    }
                    Some(Ok(heycode_trust::TrustStartupState::Prompt(_))) => {
                        if let Some(view) = self.workspace_trust.as_mut() {
                            view.error = Some(error.to_string());
                        }
                    }
                    Some(Err(refresh_error)) => {
                        if let Some(view) = self.workspace_trust.as_mut() {
                            view.error = Some(refresh_error.to_string());
                        }
                    }
                    None => {}
                }
                false
            }
            None => false,
        }
    }

    fn finish_workspace_trust(&mut self, snapshot: Arc<heycode_trust::WorkspaceTrustSnapshot>) {
        self.run_outcome = Some(TuiRunOutcome::RecomposeWorkspaceTrust {
            decision: snapshot.decision(),
            persistence: snapshot.persistence(),
            revision: snapshot.revision(),
        });
        self.workspace_trust = None;
        self.pending_send = None;
        self.pending_attachments.clear();
    }

    /// Put one approval dialog on screen, or in line behind the one already
    /// there.
    ///
    /// Parallel tool calls each park their own caller on the approval policy,
    /// and a single slot answered exactly one of them: the rest were
    /// overwritten and their callers waited forever. Every ask therefore keeps
    /// its place until a human answers it.
    fn interrupt_approval_owner(&mut self) {
        let Some(ask) = &self.pending_ask else {
            return;
        };
        if let Some(key) = &ask.owner_key {
            self.task_console
                .queue(crate::task_console::TaskAction::Interrupt(key.clone()));
        } else if ask.owner_session.is_none()
            && let Some(interrupt) = &self.interrupt_fn
        {
            interrupt();
            self.cancellation_requested = self.active_turn;
        }
        self.resolve_ask_with(heycode_agent::AskAnswer::Deny);
    }

    fn bind_pending_tool_approval(&mut self, index: usize) {
        let Some(Item::Tool { name, .. }) = self.items.get(index) else {
            return;
        };
        let edit_preview = crate::approval_preview::edit_preview_for(&self.items, index);
        let ask = self
            .pending_ask
            .iter_mut()
            .chain(self.queued_asks.iter_mut())
            .find(|ask| {
                ask.owner_session.is_none() && ask.tool_index.is_none() && ask.name == *name
            });
        if let Some(ask) = ask {
            ask.tool_index = Some(index);
            ask.edit_preview = edit_preview;
            self.set_tool_approval(Some(index), "awaiting approval".into());
        }
    }

    fn open_ask(&mut self, mut ask: PendingAskView) {
        ask.tool_index = if ask.owner_session.is_none() {
            self.items.iter().position(|item| matches!(item,
            Item::Tool { name, result: None, view, .. } if name == &ask.name && view.approval.is_none()
        ))
        } else {
            None
        };
        ask.edit_preview = ask
            .tool_index
            .and_then(|index| crate::approval_preview::edit_preview_for(&self.items, index));
        self.set_tool_approval(ask.tool_index, "awaiting approval".into());
        if self.pending_ask.is_some() {
            self.queued_asks.push_back(ask);
            return;
        }
        self.close_capability_catalog();
        self.profile_picker = None;
        self.session_browser = None;
        self.theme_picker = None;
        self.advisor_panel = None;
        self.rewind_picker = None;
        self.keymap_picker = None;
        self.help_panel = None;
        self.memory_panel = None;
        self.skill_doctor_panel = None;
        self.copy_panel = None;
        self.scroll_speed_picker = None;
        self.command_palette = None;
        self.close_permission_picker();
        self.close_model_picker();
        self.close_effort_picker();
        self.close_route_picker();
        self.close_mcp_panel();
        self.close_plugin_panel();
        self.pending_ask = Some(ask);
    }

    /// Only native approvals carry complete inputs with an exact-match grant owner.
    /// The real arguments of the call the approval card is asking about, when
    /// the card is bound to a transcript entry. Display code uses them to name
    /// the file or command instead of dumping the argument list.
    pub(crate) fn pending_ask_arguments(&self) -> Option<&serde_json::Value> {
        let index = self.pending_ask.as_ref()?.tool_index?;
        match self.items.get(index)? {
            Item::Tool { args, .. } => Some(args),
            _ => None,
        }
    }

    pub(crate) fn approval_choices(&self) -> &'static [&'static str] {
        let native = self
            .pending_ask
            .as_ref()
            .is_some_and(|ask| !self.runtime_permission_ids.contains_key(&ask.id));
        if self.approval_grants_edits() {
            &ASK_EDIT_CHOICES
        } else if self.permission == "accepted_edits" && native {
            &ASK_CHOICES
        } else {
            &ASK_DEFAULT_CHOICES
        }
    }

    pub(crate) fn approval_grants_edits(&self) -> bool {
        matches!(self.permission.as_str(), "ask" | "default")
            && self.runtime == "native"
            && self.can_cycle_approval_mode()
            && self.pending_ask.as_ref().is_some_and(|ask| {
                !self.runtime_permission_ids.contains_key(&ask.id)
                    && heycode_agent::AcceptedEdits::allows_file_tool(&ask.name)
            })
    }

    fn accept_future_edits(&mut self) {
        if self.pending_send.is_some() || !self.approval_grants_edits() {
            return;
        }
        self.pending_edit_approval_id = self.pending_ask.as_ref().map(|ask| ask.id);
        self.quiet_permission_mode = Some("accepted_edits".into());
        self.pending_send = Some("/permissions accepted_edits".into());
    }

    /// Approval dialogs waiting behind the one on screen.
    #[must_use]
    pub fn queued_ask_count(&self) -> usize {
        self.queued_asks.len()
    }

    fn set_tool_approval(&mut self, index: Option<usize>, decision: String) {
        if let Some(Item::Tool { view, .. }) = index.and_then(|index| self.items.get_mut(index)) {
            view.approval = Some(decision);
        }
    }

    /// Answer the live dialog and update its existing tool card.
    fn resolve_ask_with(&mut self, answer: heycode_agent::AskAnswer) {
        if let Some(ask) = self.pending_ask.take() {
            self.pending_ask = self.queued_asks.pop_front();
            if let Some(request_id) = self.runtime_permission_ids.remove(&ask.id) {
                self.pending_runtime_permission_response = Some((
                    request_id,
                    match answer {
                        heycode_agent::AskAnswer::Allow => {
                            heycode_app_server::AppPermissionDecision::AllowOnce
                        }
                        heycode_agent::AskAnswer::AllowSession => {
                            heycode_app_server::AppPermissionDecision::AllowSession
                        }
                        // The v1 wire carries no reason field, so a proxied
                        // denial is a plain Deny; the reason still reaches the
                        // transcript so the user sees what they said.
                        heycode_agent::AskAnswer::Deny
                        | heycode_agent::AskAnswer::DenyWithReason(_) => {
                            heycode_app_server::AppPermissionDecision::Deny
                        }
                    },
                ));
            } else if let Some(policy) = &self.approvals
                && !policy.try_answer_with(ask.id, answer.clone())
            {
                self.set_tool_approval(ask.tool_index, "cancelled".into());
                return;
            }
            self.set_tool_approval(
                ask.tool_index,
                match &answer {
                    heycode_agent::AskAnswer::Allow => "approved".into(),
                    heycode_agent::AskAnswer::AllowSession => "approved for identical calls".into(),
                    heycode_agent::AskAnswer::Deny => "rejected".into(),
                    heycode_agent::AskAnswer::DenyWithReason(reason) => {
                        format!("rejected: {reason}")
                    }
                },
            );
        }
    }

    fn take_runtime_permission_response(
        &mut self,
    ) -> Option<(String, heycode_app_server::AppPermissionDecision)> {
        self.pending_runtime_permission_response.take()
    }

    fn resolve_runtime_question(&mut self) {
        let Some(question) = self.pending_runtime_question.take() else {
            return;
        };
        let answer = if question.mode == heycode_core::QuestionMode::MultipleChoice
            && question.selection < question.choices.len()
        {
            let labels = question
                .selected_choices
                .iter()
                .filter_map(|index| question.choices.get(*index).cloned())
                .collect::<Vec<_>>();
            if labels.is_empty() {
                self.pending_runtime_question = Some(question);
                return;
            }
            heycode_agent::QuestionAnswer::Selected(labels)
        } else {
            let answer = question
                .choices
                .get(question.selection)
                .cloned()
                .unwrap_or_else(|| question.input.clone());
            if answer.trim().is_empty() {
                self.pending_runtime_question = Some(question);
                return;
            }
            heycode_agent::QuestionAnswer::Answer(answer)
        };
        // Shared host-tool results already display the answer in their card.
        // Adapter-native questions may have no matching tool card.
        if !question.request_id.starts_with(TUI_AGENT_QUESTION_PREFIX) {
            self.items.push(Item::Info(format!(
                "You answered: {}",
                question_answer_text(&answer)
            )));
        }
        self.pending_runtime_question_response = Some((question.request_id, Some(answer)));
    }

    fn cancel_runtime_question(&mut self) {
        let Some(question) = self.pending_runtime_question.take() else {
            return;
        };
        if !question.request_id.starts_with(TUI_AGENT_QUESTION_PREFIX) {
            self.items.push(Item::Info("Question cancelled".to_owned()));
        }
        self.pending_runtime_question_response = Some((question.request_id, None));
    }

    fn take_runtime_question_response(
        &mut self,
    ) -> Option<(String, Option<heycode_agent::QuestionAnswer>)> {
        self.pending_runtime_question_response.take()
    }
}

fn question_answer_text(answer: &heycode_agent::QuestionAnswer) -> String {
    match answer {
        heycode_agent::QuestionAnswer::Answer(text) => text.clone(),
        heycode_agent::QuestionAnswer::Selected(labels) => labels.join(", "),
        heycode_agent::QuestionAnswer::Cancelled => String::new(),
    }
}

fn append_stream(
    items: &mut Vec<Item>,
    is_open: impl Fn(&Item) -> bool,
    make: impl Fn() -> Item,
    push: impl Fn(&mut Item),
) {
    let open = items.last_mut().filter(|i| is_open(i));
    if let Some(last) = open {
        push(last);
    } else {
        let mut item = make();
        push(&mut item);
        items.push(item);
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0)
}

/// Bridge the bus into an unbounded channel for the select loop.
#[must_use]
pub fn ui_channel(bus: &EventBus) -> tokio::sync::mpsc::UnboundedReceiver<UiEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    bus.on::<UiEvent>(move |e| {
        let _ = tx.send(e.clone());
    });
    rx
}

fn app_server_owned_ui_event(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::TurnStarted { .. }
            | UiEvent::RuntimeTurnStarted { .. }
            | UiEvent::UserEcho { .. }
            | UiEvent::UserAttachmentsEcho { .. }
            | UiEvent::AssistantDelta { .. }
            | UiEvent::AssistantAudio { .. }
            | UiEvent::ReasoningDelta { .. }
            | UiEvent::ContextBudgetChanged { .. }
            | UiEvent::RuntimeContextMeasured { .. }
            | UiEvent::TurnFinished { .. }
    )
}

fn session_owned_ui_event(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::UserEcho { .. }
            | UiEvent::UserAttachmentsEcho { .. }
            | UiEvent::AssistantDelta { .. }
            | UiEvent::AssistantAudio { .. }
            | UiEvent::ReasoningDelta { .. }
            | UiEvent::ToolStarted { .. }
            | UiEvent::ToolFinished { .. }
            | UiEvent::FindingsReported { .. }
    )
}

fn app_server_ui_events(event: heycode_app_server::AppServerEvent) -> Vec<UiEvent> {
    match event {
        heycode_app_server::AppServerEvent::UserInput {
            text,
            attachments,
            document_routes,
        } => {
            let mut events = Vec::new();
            if !attachments.is_empty() {
                events.push(UiEvent::UserAttachmentsEcho {
                    attachments,
                    document_routes,
                });
            }
            events.push(UiEvent::UserEcho { text });
            events
        }
        heycode_app_server::AppServerEvent::TurnStarted { turn_id } => {
            vec![UiEvent::RuntimeTurnStarted { turn_id }]
        }
        heycode_app_server::AppServerEvent::AssistantDelta { text } => {
            vec![UiEvent::AssistantDelta { text }]
        }
        heycode_app_server::AppServerEvent::AssistantAudio { attachments } => {
            vec![UiEvent::AssistantAudio { attachments }]
        }
        heycode_app_server::AppServerEvent::ReasoningDelta { text } => {
            vec![UiEvent::ReasoningDelta { text }]
        }
        heycode_app_server::AppServerEvent::ToolStarted { .. }
        | heycode_app_server::AppServerEvent::ToolFinished { .. } => Vec::new(),
        heycode_app_server::AppServerEvent::ContextBudgetChanged { budget } => {
            vec![UiEvent::ContextBudgetChanged { budget: *budget }]
        }
        heycode_app_server::AppServerEvent::Usage { context, .. } => context
            .map(|context| {
                vec![UiEvent::RuntimeContextMeasured {
                    resolved_model: context.resolved_model,
                    tokens: context.tokens,
                    context_window: context.context_window,
                }]
            })
            .unwrap_or_default(),
        heycode_app_server::AppServerEvent::PlanChanged { .. } => Vec::new(),
        heycode_app_server::AppServerEvent::Notice { .. } => Vec::new(),
        heycode_app_server::AppServerEvent::AuthorizationPromptRequested { .. }
        | heycode_app_server::AppServerEvent::AuthorizationPromptResolved { .. } => Vec::new(),
        heycode_app_server::AppServerEvent::PermissionRequested {
            request_id,
            action,
            detail,
        } => vec![UiEvent::RuntimePermissionRequested {
            request_id,
            action,
            detail,
        }],
        heycode_app_server::AppServerEvent::QuestionRequested {
            request_id,
            header,
            prompt,
            choices,
            choice_descriptions,
            mode,
            progress,
        } => vec![UiEvent::RuntimeQuestionRequested {
            request_id,
            header,
            prompt,
            choices,
            choice_descriptions,
            mode,
            progress,
        }],
        heycode_app_server::AppServerEvent::TurnFinished { reason, usage, .. } => {
            vec![UiEvent::TurnFinished {
                reason: match reason {
                    heycode_app_server::AppTurnReason::Stop => "stop",
                    heycode_app_server::AppTurnReason::Limit => "max_tokens",
                    heycode_app_server::AppTurnReason::Cancelled => "aborted",
                    heycode_app_server::AppTurnReason::Error => "error",
                }
                .to_owned(),
                usage,
                context_tokens: None,
            }]
        }
    }
}

async fn run_app_server_turn(
    client: heycode_app_server::LocalAppClient,
    text: String,
    attachments: Vec<heycode_core::AttachmentMetadata>,
    cancellation: CancellationToken,
    ui: tokio::sync::mpsc::UnboundedSender<UiEvent>,
) -> anyhow::Result<heycode_app_server::AppTurnResult> {
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel(256);
    let turn = client.turn(&text, attachments, events_tx, cancellation);
    tokio::pin!(turn);
    let mut result = None;
    loop {
        if result.is_some() && events_rx.is_closed() && events_rx.is_empty() {
            break;
        }
        tokio::select! {
            value = &mut turn, if result.is_none() => result = Some(value),
            event = events_rx.recv() => {
                match event {
                    Some(notification) => {
                        for event in app_server_ui_events(notification.params.event) {
                            let _sent = ui.send(event);
                        }
                    }
                    None if result.is_some() => break,
                    None => {}
                }
            }
        }
    }
    result
        .ok_or_else(|| anyhow::anyhow!("app-server turn did not settle"))?
        .map_err(anyhow::Error::new)
}

enum TuiTurnResult {
    AppServer(heycode_app_server::AppTurnResult),
    NativeFollowUp(heycode_agent::TurnReport),
}

impl TuiTurnResult {
    fn is_error(&self) -> bool {
        match self {
            Self::AppServer(result) => {
                matches!(result.reason, heycode_app_server::AppTurnReason::Error)
            }
            Self::NativeFollowUp(result) => result.reason == "error",
        }
    }
}

fn submit_tui_inbox(
    agent: &heycode_agent::Agent,
    delivery: heycode_session::InboxDelivery,
    text: &str,
) -> anyhow::Result<(heycode_session::InboxDelivery, heycode_agent::InboxWake)> {
    let (id, wake) = agent.submit_inbox(delivery, text)?;
    if delivery != heycode_session::InboxDelivery::Steer || wake != heycode_agent::InboxWake::Wake {
        return Ok((delivery, wake));
    }

    // The visual turn can settle between the key event and this durable
    // append. An idle next-step steer has no turn to drain it, so preserve the
    // human intent as a next-turn follow-up instead of stranding it. The
    // canceled steer remains truthful durable race evidence.
    if !agent.cancel_inbox(&id)? {
        anyhow::bail!("steer settled before its idle-race conversion");
    }
    let (_, wake) = agent.submit_inbox(heycode_session::InboxDelivery::FollowUp, text)?;
    Ok((heycode_session::InboxDelivery::FollowUp, wake))
}

async fn run_native_follow_up(
    agent: Arc<heycode_agent::Agent>,
    cancellation: CancellationToken,
) -> anyhow::Result<TuiTurnResult> {
    let id = agent
        .next_wakeable_message()
        .ok_or(heycode_agent::FollowUpError::Empty)?;
    agent
        .send_inbox_id_cancellable(&id, cancellation)
        .await
        .map(TuiTurnResult::NativeFollowUp)
}

const TUI_AGENT_QUESTION_PREFIX: &str = "tui-agent-question-";

fn tui_agent_question_id(owner: u64, id: u64) -> String {
    format!("{TUI_AGENT_QUESTION_PREFIX}{owner}-{id}")
}

fn parse_tui_agent_question_id(request_id: &str) -> Option<(u64, u64)> {
    let value = request_id.strip_prefix(TUI_AGENT_QUESTION_PREFIX)?;
    let (owner, id) = value.split_once('-')?;
    Some((owner.parse().ok()?, id.parse().ok()?))
}

fn spawn_workspace_context_probe(
    subprocess: heycode_exec::SubprocessService,
    cwd: std::path::PathBuf,
    cancellation: CancellationToken,
) -> tokio::task::JoinHandle<crate::workspace_context::WorkspaceContextState> {
    tokio::spawn(
        async move { crate::workspace_context::probe(subprocess, &cwd, cancellation).await },
    )
}

/// A recomposition message with its original success or failure meaning.
#[derive(Clone, Debug)]
pub enum StartupNotice {
    /// A local command whose empty-conversation transition has completed.
    Command(String),
    /// A local command and successful receipt, kept out of durable model history.
    CommandResult {
        /// Validated display command.
        command: String,
        /// Result produced before recomposition.
        message: String,
    },
    /// A successful user action completed before the terminal restarted.
    Info(String),
    /// A failure or cleanup problem needs attention.
    Error(String),
}

/// Everything the interactive loop needs.
pub struct LoopDeps {
    /// The composed agent.
    pub agent: Arc<heycode_agent::Agent>,
    /// Stable local app-server used for foreground TUI turns.
    pub app_server: Arc<heycode_app_server::AppServer>,
    /// Exclusive human-question owner while this terminal is active.
    pub questions: Arc<heycode_agent::InteractiveQuestion>,
    /// Slash-command registry.
    pub commands: Arc<CommandRegistry>,
    /// Layered settings/CAS service used by U14.
    pub settings: Arc<heycode_settings::SettingsService>,
    /// Effect-owned custom/derived settings-surface registry.
    pub settings_ui: Arc<heycode_ui::settings_ui::SettingsUiRegistry>,
    /// MCP definition operations mounted for the U12 panel.
    pub mcp_management: Option<Arc<heycode_mcp::management::McpManagement>>,
    /// Exact session product control for named-server reconnect operations.
    pub mcp_runtime_control: Option<Arc<heycode_mcp::McpRuntimeControl>>,
    /// Live MCP connection registry, when the profile mounted one.
    pub mcp_registry: Option<Arc<heycode_mcp::McpRegistry>>,
    /// Installed-plugin lifecycle owner for U13.
    pub plugin_lifecycle: Option<Arc<heycode_extensions::lifecycle::PluginLifecycle>>,
    /// Safe package/provenance projection, when an installer supplied one.
    pub plugin_packages: Option<Arc<PluginPackageIndex>>,
    /// Trusted instruction and memory sources for the native chooser.
    pub memory_sources: Option<Arc<crate::memory_commands::MemorySourceManager>>,
    /// Exact attachment owner used only for explicit saves of delivered files.
    pub attachments: Option<Arc<heycode_attachments::AttachmentStore>>,
    /// Discovered skills service for CMD04.
    pub skills: Option<Arc<heycode_skills::SkillSet>>,
    /// Delegation registry for CMD04.
    pub subagents: Option<Arc<heycode_agent::SubagentRegistry>>,
    /// Effect-owned hook registry for CMD04.
    pub hooks: Option<Arc<heycode_hooks::HookService>>,
    /// Effect-owned background jobs for U22/O05.
    pub jobs: Option<Arc<heycode_agent::JobRegistry>>,
    /// Effect-owned subprocess boundary for cached workspace identity probes.
    pub subprocess: heycode_exec::SubprocessService,
    /// Working directory shown in the status line.
    pub cwd: std::path::PathBuf,
    /// Typed unknown-workspace prompt bound to the live trust service before
    /// any project input was discovered.
    pub workspace_trust: Option<heycode_trust::WorkspaceTrustPrompt>,
    /// Present when the composition selected ask-mode approvals.
    pub approvals: Option<std::sync::Arc<heycode_agent::InteractiveApproval>>,
    /// Model context window (drives ctx % in status bar; 0 hides it).
    pub context_window: u64,
    /// Fraction of the window at which the status meter warns.
    pub context_warn_ratio: f32,
    /// Plugin-owned first-run/connect state.
    pub onboarding: Option<Arc<heycode_onboarding::OnboardingService>>,
    /// Interactive masked secret broker.
    pub secret_prompt: Option<Arc<heycode_authorization_api_key::InteractiveSecretPrompt>>,
    /// Safe authorization flow catalog.
    pub authorization: Option<Arc<heycode_authorization::AuthorizationService>>,
    /// Plugin-contributed health registry for the welcome card.
    pub doctor: Option<Arc<heycode_doctor::DoctorRegistry>>,
    /// Live provider model catalogs for the picker.
    pub models: Option<Arc<heycode_llm::CatalogRegistry>>,
    /// Optional layered user/project assertions shown beside provider evidence.
    pub catalog_overrides: Option<Arc<heycode_catalog_file::CatalogOverrides>>,
    /// Live native/delegated agent runtimes for the combined route picker.
    pub runtimes: Arc<heycode_runtime::AgentRuntimeRegistry>,
    /// Durable-before-live runtime/provider/model selection owner.
    pub routing: Arc<heycode_routing::RoutingService>,
    /// Effect-owned strict named-profile picker boundary.
    pub profiles: Arc<heycode_config::NamedProfileService>,
    /// Profile selected by the current CLI composition, when any.
    pub current_profile: Option<String>,
    /// Where to keep prompts so Up recalls them in the next run. `None` on a
    /// surface with no durable home.
    pub prompt_history_path: Option<std::path::PathBuf>,
    /// A message the composition root wants shown first — typically why a
    /// requested recomposition (`/resume`, `/fork`, `/profile`) was abandoned
    /// and the previous world restored.
    pub startup_notice: Option<StartupNotice>,
    /// Non-fatal startup findings (unknown config keys, an unreachable
    /// credential check) — shown once, after the session line, because
    /// stderr is invisible behind the alternate screen.
    pub startup_warnings: Vec<String>,
}

/// Persisted UI values resolved by the TuiHandle from its captured UI/Settings
/// registries before the loop starts.
pub(crate) struct StartupUiPreferences {
    pub(crate) theme: heycode_ui::theme::Theme,
    pub(crate) keymap: heycode_ui::keymap::Keymap,
    pub(crate) vim_mode: bool,
    pub(crate) focus_view: bool,
    pub(crate) shell: heycode_ui::preferences::ShellChromePreferences,
    pub(crate) scroll_speed: f32,
}

/// TUI-handle-owned control bridges and startup UI values.
pub(crate) struct InteractiveSurfaces {
    pub(crate) panels: PanelCommandBridge,
    pub(crate) human_commands: crate::human_commands::HumanCommandBridge,
    pub(crate) advisor_bridge: crate::advisor_panel::AdvisorPanelBridge,
    pub(crate) advisor_control: Option<(
        Arc<heycode_agent::AdvisorService>,
        Arc<heycode_agent::Agent>,
    )>,
    pub(crate) recomposition: crate::recomposition::RecompositionBridge,
    pub(crate) add_directory: Option<crate::add_directory::AddDirectoryBridge>,
    pub(crate) startup_preferences: Option<StartupUiPreferences>,
    pub(crate) session_events: crate::transcript::SessionEventBridge,
    pub(crate) execution_jobs: Option<Arc<heycode_agent::ExecutionJobService>>,
    pub(crate) workflows: Option<Arc<heycode_agent::WorkflowService>>,
    pub(crate) mcp_client: Option<Arc<crate::product_attachments::McpTuiBridge>>,
    /// What the entered screen asked of this terminal's input. Capabilities
    /// alone cannot say it for a flat frame the user requested on a capable
    /// terminal.
    pub(crate) input: crate::terminal::InputProtocol,
}

type AuthorizationResult =
    Result<heycode_authorization::AuthorizationReceipt, heycode_authorization::AuthorizationError>;

struct AssistantConnectionOperation {
    runtime: String,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<
        Result<Option<heycode_llm::CatalogSnapshot>, heycode_runtime::RuntimeError>,
    >,
}

struct ProviderConnectionCatalog {
    provider: String,
    endpoint: Option<String>,
    parameters: BTreeMap<String, String>,
    credential_reference: Option<heycode_credentials::CredentialReference>,
    snapshot: Option<heycode_llm::CatalogSnapshot>,
}

struct ProviderConnectionOperation {
    provider: String,
    endpoint: Option<String>,
    parameters: BTreeMap<String, String>,
    credential_reference: Option<heycode_credentials::CredentialReference>,
    credential_verified: Arc<std::sync::atomic::AtomicBool>,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<Result<heycode_llm::CatalogView, heycode_llm::CatalogError>>,
}

type ModelRefreshOutcome = (
    BackendControlOwner,
    Result<heycode_llm::CatalogView, String>,
);
type ModelEffortOutcome = (
    BackendControlOwner,
    u64,
    String,
    Result<heycode_routing::BackendEffortCatalog, String>,
);

struct ModelConfigurationOperation {
    owner: BackendControlOwner,
    revision: u64,
    model: String,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<Result<heycode_runtime::RuntimeModelConfiguration, String>>,
}

impl ModelConfigurationOperation {
    fn start(
        routing: Arc<heycode_routing::RoutingService>,
        active: &heycode_routing::ActiveRoutingConfiguration,
        lifecycle: &CancellationToken,
    ) -> Option<Self> {
        let BackendControlOwner::DelegatedRuntime { .. } = active.owner() else {
            return None;
        };
        let model = active.model()?.to_owned();
        let owner = active.owner().clone();
        let revision = active.revision();
        let cancellation = lifecycle.child_token();
        let task_owner = owner.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            routing
                .model_configuration_owned(&task_owner, revision, task_cancellation)
                .await
                .map_err(|error| error.to_string())
        });
        Some(Self {
            owner,
            revision,
            model,
            cancellation,
            task,
        })
    }

    fn matches(&self, active: &heycode_routing::ActiveRoutingConfiguration) -> bool {
        self.owner == *active.owner()
            && self.revision == active.revision()
            && active.model() == Some(self.model.as_str())
    }

    fn cancel(self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

impl ProviderConnectionOperation {
    fn start_parameters_authenticated(
        provider: String,
        parameters: BTreeMap<String, String>,
        reference: heycode_credentials::CredentialReference,
        models: Arc<heycode_llm::CatalogRegistry>,
        authorization: Arc<heycode_authorization::AuthorizationService>,
        prompt: Arc<heycode_authorization_api_key::InteractiveSecretPrompt>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let target = provider.clone();
        let draft = parameters.clone();
        let credential = reference.clone();
        let task = tokio::spawn(async move {
            crate::endpoint_connection::authorize_parameters(
                models,
                authorization,
                prompt,
                target,
                draft,
                credential,
                token,
            )
            .await
            .map(|snapshot| heycode_llm::CatalogView {
                snapshot: Arc::new(snapshot),
                freshness: heycode_llm::CatalogFreshness::Live,
                warning: None,
            })
        });
        Self {
            provider,
            endpoint: None,
            parameters,
            credential_reference: Some(reference),
            credential_verified: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cancellation,
            task,
        }
    }

    fn start_parameters(
        provider: String,
        parameters: BTreeMap<String, String>,
        credential_reference: Option<heycode_credentials::CredentialReference>,
        models: Arc<heycode_llm::CatalogRegistry>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let target = provider.clone();
        let draft = parameters.clone();
        let task = tokio::spawn(async move {
            models
                .probe_parameters(&target, &draft, token)
                .await
                .map(|snapshot| heycode_llm::CatalogView {
                    snapshot: Arc::new(snapshot),
                    freshness: heycode_llm::CatalogFreshness::Live,
                    warning: None,
                })
        });
        Self {
            provider,
            endpoint: None,
            parameters,
            credential_reference,
            credential_verified: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cancellation,
            task,
        }
    }

    fn start_authenticated(
        provider: String,
        endpoint: String,
        models: Arc<heycode_llm::CatalogRegistry>,
        authorization: Arc<heycode_authorization::AuthorizationService>,
        prompt: Arc<heycode_authorization_api_key::InteractiveSecretPrompt>,
    ) -> anyhow::Result<Self> {
        let reference = heycode_credentials::CredentialReference::new(format!(
            "HEYCODE_ENDPOINT_{}",
            uuid::Uuid::new_v4().simple()
        ))?;
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let target = provider.clone();
        let address = endpoint.clone();
        let credential = reference.clone();
        let task = tokio::spawn(async move {
            crate::endpoint_connection::authorize_endpoint(
                models,
                authorization,
                prompt,
                target,
                address,
                credential,
                token,
            )
            .await
            .map(|snapshot| heycode_llm::CatalogView {
                snapshot: Arc::new(snapshot),
                freshness: heycode_llm::CatalogFreshness::Live,
                warning: None,
            })
        });
        Ok(Self {
            provider,
            endpoint: Some(endpoint),
            parameters: BTreeMap::new(),
            credential_reference: Some(reference),
            credential_verified: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cancellation,
            task,
        })
    }

    fn start(
        provider: String,
        models: Arc<heycode_llm::CatalogRegistry>,
        endpoint: Option<String>,
        credential_already_verified: bool,
        validation: Option<(
            Arc<heycode_authorization::AuthorizationService>,
            heycode_authorization::AuthorizationFlowId,
        )>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let target = provider.clone();
        let draft_endpoint = endpoint.clone();
        let credential_verified = Arc::new(std::sync::atomic::AtomicBool::new(
            credential_already_verified,
        ));
        let verified = credential_verified.clone();
        let task = tokio::spawn(async move {
            if let Some((authorization, flow)) = validation {
                authorization.validate_existing(&flow, token.clone()).await.map_err(|error| {
                    let rejected = matches!(&error, heycode_authorization::AuthorizationError::Flow { code, .. } if code == "unauthorized");
                    heycode_llm::CatalogError::Refresh {
                        provider: target.clone(),
                        kind: if rejected { heycode_llm::CatalogFailureKind::Unauthorized } else { heycode_llm::CatalogFailureKind::Network },
                        message: if rejected { "The API key is invalid or has been revoked. Enter a valid key to retry.".into() } else { "Could not validate the API key. Check your connection and retry.".into() },
                    }
                })?;
                verified.store(true, std::sync::atomic::Ordering::Release);
            }
            if let Some(endpoint) = draft_endpoint {
                return models
                    .probe_endpoint(&target, &endpoint, token)
                    .await
                    .map(|snapshot| heycode_llm::CatalogView {
                        snapshot: Arc::new(snapshot),
                        freshness: heycode_llm::CatalogFreshness::Live,
                        warning: None,
                    });
            }
            models
                .refresh(&target, heycode_llm::CatalogRefreshMode::Force, token)
                .await
        });
        Self {
            provider,
            endpoint,
            parameters: BTreeMap::new(),
            credential_reference: None,
            credential_verified,
            cancellation,
            task,
        }
    }
}

fn catalog_credential_rejected(
    result: &Result<heycode_llm::CatalogView, heycode_llm::CatalogError>,
) -> bool {
    let error = match result {
        Ok(view) => view.warning.as_ref(),
        Err(error) => Some(error),
    };
    matches!(
        error,
        Some(heycode_llm::CatalogError::Refresh {
            kind: heycode_llm::CatalogFailureKind::Unauthorized,
            ..
        })
    )
}

fn catalog_allows_explicit_model(error: &heycode_llm::CatalogError) -> bool {
    matches!(
        error,
        heycode_llm::CatalogError::Refresh {
            kind: heycode_llm::CatalogFailureKind::Network
                | heycode_llm::CatalogFailureKind::Unavailable,
            ..
        }
    )
}

struct AuthorizationOperation {
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<AuthorizationResult>,
}

/// How long shutdown waits for a cancelled task to settle on its own.
const SHUTDOWN_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long shutdown then waits for the abort to take effect.
const SHUTDOWN_ABORT_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Cancel a background task and wait for it, but never forever.
///
/// Quit runs through here, so a task that ignores its token — a provider
/// stream with no deadline, a runtime that stopped answering — would
/// otherwise hold the terminal in raw mode with no way out. The task is
/// aborted once the wait expires and detached if even the abort does not
/// land, so the quit path is bounded whatever the task does.
async fn cancel_and_join<T>(
    cancellation: Option<CancellationToken>,
    task: &mut Option<tokio::task::JoinHandle<T>>,
) {
    cancel_and_join_within(
        cancellation,
        task,
        SHUTDOWN_JOIN_TIMEOUT,
        SHUTDOWN_ABORT_GRACE,
    )
    .await;
}

async fn cancel_and_join_within<T>(
    cancellation: Option<CancellationToken>,
    task: &mut Option<tokio::task::JoinHandle<T>>,
    settle: std::time::Duration,
    grace: std::time::Duration,
) {
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
    }
    if let Some(mut handle) = task.take()
        && tokio::time::timeout(settle, &mut handle).await.is_err()
    {
        handle.abort();
        let _abandoned = tokio::time::timeout(grace, &mut handle).await;
    }
}

async fn settle_authorization(operation: &mut Option<AuthorizationOperation>) {
    if let Some(operation) = operation.take() {
        let mut task = Some(operation.task);
        cancel_and_join(Some(operation.cancellation), &mut task).await;
    }
}

async fn finish_authorization<T>(
    operation: &mut Option<AuthorizationOperation>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    settle_authorization(operation).await;
    result
}

/// Run the interactive session, calling `draw_fn` once per loop iteration.
/// Generic over the drawer so tests drive it with TestBackend frames.
///
/// The shell owns a private panel-open inbox: nothing outside this loop can
/// ask it to open a capability panel. [`run_interactive_with`] is the entry
/// point that shares one.
///
/// # Errors
/// Terminal event stream failures.
pub async fn run_interactive<D>(deps: LoopDeps, draw_fn: D) -> anyhow::Result<TuiRunOutcome>
where
    D: FnMut(&mut AppState),
{
    run_interactive_internal(
        deps,
        InteractiveSurfaces {
            panels: PanelCommandBridge::new(),
            human_commands: crate::human_commands::HumanCommandBridge::new(),
            advisor_bridge: crate::advisor_panel::AdvisorPanelBridge::new(),
            advisor_control: None,
            recomposition: crate::recomposition::RecompositionBridge::default(),
            add_directory: None,
            startup_preferences: None,
            session_events: crate::transcript::SessionEventBridge::default(),
            execution_jobs: None,
            workflows: None,
            mcp_client: None,
            input: crate::terminal::InputProtocol::Escapes,
        },
        None,
        None,
        draw_fn,
    )
    .await
}

/// Run the interactive session over a panel-open inbox shared with the CMD04
/// capability commands.
///
/// # Errors
/// Terminal event stream failures.
pub async fn run_interactive_with<D>(
    deps: LoopDeps,
    panels: PanelCommandBridge,
    draw_fn: D,
) -> anyhow::Result<TuiRunOutcome>
where
    D: FnMut(&mut AppState),
{
    run_interactive_internal(
        deps,
        InteractiveSurfaces {
            panels,
            human_commands: crate::human_commands::HumanCommandBridge::new(),
            advisor_bridge: crate::advisor_panel::AdvisorPanelBridge::new(),
            advisor_control: None,
            recomposition: crate::recomposition::RecompositionBridge::default(),
            add_directory: None,
            startup_preferences: None,
            session_events: crate::transcript::SessionEventBridge::default(),
            execution_jobs: None,
            workflows: None,
            mcp_client: None,
            input: crate::terminal::InputProtocol::Escapes,
        },
        None,
        None,
        draw_fn,
    )
    .await
}

pub(crate) async fn run_interactive_with_capabilities<D>(
    deps: LoopDeps,
    surfaces: InteractiveSurfaces,
    capabilities: heycode_ui::terminal::TerminalCapabilities,
    draw_fn: D,
) -> anyhow::Result<TuiRunOutcome>
where
    D: FnMut(&mut AppState),
{
    run_interactive_internal(deps, surfaces, None, Some(capabilities), draw_fn).await
}

pub(crate) async fn run_interactive_with_sessions_and_capabilities<D>(
    deps: LoopDeps,
    surfaces: InteractiveSurfaces,
    session_query: Arc<heycode_session::SessionQueryService>,
    session_commands: crate::session_browser::SessionCommandBridge,
    capabilities: heycode_ui::terminal::TerminalCapabilities,
    draw_fn: D,
) -> anyhow::Result<TuiRunOutcome>
where
    D: FnMut(&mut AppState),
{
    run_interactive_internal(
        deps,
        surfaces,
        Some((session_query, session_commands)),
        Some(capabilities),
        draw_fn,
    )
    .await
}

async fn run_interactive_internal<D>(
    deps: LoopDeps,
    surfaces: InteractiveSurfaces,
    session_support: Option<(
        Arc<heycode_session::SessionQueryService>,
        crate::session_browser::SessionCommandBridge,
    )>,
    terminal_capabilities: Option<heycode_ui::terminal::TerminalCapabilities>,
    mut draw_fn: D,
) -> anyhow::Result<TuiRunOutcome>
where
    D: FnMut(&mut AppState),
{
    let session_events = surfaces.session_events.clone();
    let mcp_bridge = surfaces.mcp_client.clone();
    let selection = deps.agent.selection();
    let active_configuration = deps.routing.active_configuration().ok();
    let effective_runtime = active_configuration.as_ref().map_or_else(
        || deps.agent.runtime_id().to_owned(),
        |active| match active.owner() {
            BackendControlOwner::NativeInference { .. } => deps.agent.runtime_id().to_owned(),
            BackendControlOwner::DelegatedRuntime { runtime } => runtime.clone(),
        },
    );
    let effective_provider = active_configuration.as_ref().map_or_else(
        || selection.provider_name.clone(),
        |active| match active.owner() {
            BackendControlOwner::NativeInference { provider } => provider.clone(),
            BackendControlOwner::DelegatedRuntime { runtime } => runtime.clone(),
        },
    );
    let effective_model = active_configuration
        .as_ref()
        .and_then(|active| active.model())
        .unwrap_or_default()
        .to_owned();
    let mut workspace = deps.agent.cwd();
    let mut state = AppState::new(effective_model.clone(), workspace.clone());
    state.set_configured_reasoning_effort(
        active_configuration
            .as_ref()
            .and_then(|active| active.effort())
            .map(str::to_owned),
    );
    state.set_catalog_overrides(deps.catalog_overrides.clone());
    if let Some(workspace_trust) = deps.workspace_trust.clone() {
        state.receive_workspace_trust(workspace_trust);
    }
    state.set_welcome(WelcomeStatusView::new(
        &effective_runtime,
        effective_provider,
        effective_model,
        deps.agent.approval_kind().as_str(),
        workspace.clone(),
    ));
    if deps.doctor.is_none() {
        state.set_welcome_health(WelcomeHealth::Unavailable);
    }
    state.set_commands(deps.commands.clone());
    state.memory_sources = deps.memory_sources.clone();
    let add_directory_bridge = surfaces.add_directory.clone();
    if state.native_inbox_available() {
        match deps.agent.take_rewind_draft() {
            Ok(Some(draft)) => state.replace_input(draft),
            Ok(None) => {}
            Err(error) => state.apply(&UiEvent::Error {
                message: format!("rewind draft could not be restored: {error}"),
            }),
        }
    }
    state.set_human_commands(surfaces.human_commands);
    surfaces.advisor_bridge.attach();
    state.advisor_bridge = surfaces.advisor_bridge;
    state.advisor_control = surfaces.advisor_control;
    state.set_profiles(deps.profiles.clone(), deps.current_profile.clone());
    state.set_panel_commands(surfaces.panels);
    state.set_settings_services(deps.settings.clone(), deps.settings_ui.clone());
    if let Some(management) = deps.mcp_management.clone() {
        state.set_mcp_services(management, deps.mcp_registry.clone());
    }
    if let Some(control) = deps.mcp_runtime_control.clone() {
        state.set_mcp_runtime_control(control);
    }
    if let Some(lifecycle) = deps.plugin_lifecycle.clone() {
        state.set_plugin_services(lifecycle, deps.plugin_packages.clone());
    }
    state.set_capability_services(
        deps.skills.clone(),
        deps.subagents.clone(),
        deps.hooks.clone(),
    );
    state.set_job_registry(deps.jobs.clone());
    let task_source: Arc<dyn crate::task_console::TaskSource> = Arc::new(
        crate::task_console::RegistryTaskSource::new(
            deps.agent.clone(),
            deps.jobs.clone(),
            deps.subagents.clone(),
        )
        .map_err(anyhow::Error::msg)?
        .with_execution(surfaces.execution_jobs.clone())
        .with_commands(deps.commands.clone()),
    );
    state.set_task_source(task_source.clone());
    // This session boundary also scopes job-notice presentation when the
    // optional workflow service is unavailable.
    state.workflow_console.first_local_seq = deps
        .agent
        .session()
        .lock()
        .map_err(|_| anyhow::anyhow!("Session unavailable"))?
        .first_local_seq();
    if let (Some(service), Some(registry)) = (surfaces.workflows.clone(), deps.subagents.as_ref()) {
        let owner = {
            let session = deps
                .agent
                .session()
                .lock()
                .map_err(|_| anyhow::anyhow!("Workflow session unavailable"))?;
            heycode_agent::SubagentId::new(session.id().as_str()).map_err(anyhow::Error::msg)?
        };
        state.set_workflow_source(Arc::new(
            crate::workflow_source::ServiceWorkflowSource::new(
                service,
                registry.root_authority(owner),
                task_source,
            ),
        ));
    }
    if let Some((session_query, _)) = session_support.as_ref()
        && let Ok(metadata) = heycode_session::SessionCreationMetadata::new(
            Some(workspace.clone()),
            Some(effective_runtime.clone()),
            heycode_session::SessionSource::Interactive,
        )
    {
        state.set_session_service(
            session_query.clone(),
            deps.agent.session().clone(),
            metadata,
        );
    }
    // U20: the one place with a real terminal to look at. Everything below
    // renders from the resolved tier, so a terminal that advertised no 24-bit
    // support never receives a 24-bit escape.
    {
        let capabilities = terminal_capabilities.unwrap_or_else(|| {
            heycode_ui::terminal::TerminalCapabilities::detect(
                &crate::terminal::detect_environment(),
            )
        });
        if let Ok(theme) = heycode_ui::theme::default_theme() {
            state.apply_terminal(capabilities, &theme);
        }
        if let Some(preferences) = surfaces.startup_preferences {
            state.apply_terminal(capabilities, &preferences.theme);
            state.set_keymap(preferences.keymap);
            state.set_vim_mode(preferences.vim_mode);
            state.focus_view = preferences.focus_view;
            state.set_shell_preferences(preferences.shell);
            state.set_scroll_speed(preferences.scroll_speed);
        }
        state.set_input_protocol(surfaces.input);
    }
    state.context_window = active_configuration
        .as_ref()
        .map_or(effective_runtime == deps.agent.runtime_id(), |active| {
            active.owner().is_native()
        })
        .then_some(deps.context_window)
        .filter(|window| *window > 0);
    let (mut mcp_rx, _mcp_activation) = match mcp_bridge.as_ref() {
        Some(bridge) => {
            let (receiver, activation) = bridge
                .activate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            (Some(receiver), Some(activation))
        }
        None => (None, None),
    };
    // Subscribe while holding the session lock, then snapshot. Appends need
    // the same lock, so no durable event can land between replay and the live
    // receiver registration.
    let mut session_rx;
    {
        let session = deps
            .agent
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        session_rx = session_events.subscribe();
        if !session.events().is_empty() {
            state.replay(session.events());
        }
        // Recomposition receipts retain their severity across terminal restart.
        if let Some(notice) = deps.startup_notice.clone() {
            match notice {
                StartupNotice::Command(command) => state.items.push(Item::Command(command)),
                StartupNotice::CommandResult { command, message } => {
                    state.items.push(Item::Command(command));
                    state.items.push(Item::Info(message));
                }
                StartupNotice::Info(message) => state.items.push(Item::Info(message)),
                StartupNotice::Error(message) => state.items.push(Item::Error(message)),
            }
        }
        for warning in &deps.startup_warnings {
            state.items.push(Item::Error(warning.clone()));
        }
        // `--model x` with a different persisted route: say so once, so the
        // status line is not a mystery.
        if let Some(notice) = deps.routing.override_notice() {
            state.items.push(Item::Info(notice.to_owned()));
        }
    }
    let pending = deps.agent.pending_inbox();
    state.apply(&UiEvent::InboxUpdated {
        next_turn: pending.next_turn,
        next_step: pending.next_step,
        wake: if pending.next_turn > 0 {
            heycode_agent::InboxWake::Wake
        } else {
            heycode_agent::InboxWake::Queued
        },
    });
    let app_client = heycode_app_server::LocalAppClient::new(deps.app_server.clone());
    let connection_pending = deps
        .onboarding
        .as_ref()
        .map(|service| service.snapshot())
        .transpose()?
        .is_some_and(|view| view.active);
    let app_opened = state.workspace_trust().is_none() && !connection_pending;
    let mut live_model_configuration_applied = false;
    if app_opened {
        let opened = app_client
            .open()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // Opening an older conversation makes it the next --continue target,
        // even when the user only reads it and sends no further message.
        {
            let mut session = deps
                .agent
                .session()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !heycode_session::is_unused_session(session.events()) {
                session.append(heycode_session::SessionEventKind::SessionActivated {})?;
            }
        }
        if opened.runtime_id == state.runtime
            && let Some(configuration) = opened.model_configuration.as_ref()
            && configuration.model == state.model
        {
            state.apply_runtime_model_configuration(configuration);
            live_model_configuration_applied = true;
        }
    }
    if state.context_budget.as_ref().is_some_and(|budget| {
        budget.provider != state.provider
            || (budget.model != state.model
                && Some(budget.model.as_str()) != state.resolved_model.as_deref())
    }) {
        state.context_budget = None;
        state.context_tokens = None;
    }
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut question_rx = deps
        .questions
        .take_subscription()
        .ok_or_else(|| anyhow::anyhow!("interactive question surface is already owned"))?;
    let question_owner = question_rx.owner_id();
    let app_turn_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let direct_ui = ui_tx.clone();
    let direct_app_turn = app_turn_active.clone();
    deps.agent.ui().on::<UiEvent>(move |event| {
        if !direct_app_turn.load(std::sync::atomic::Ordering::SeqCst)
            || !app_server_owned_ui_event(event)
        {
            let _sent = direct_ui.send(event.clone());
        }
    });
    let agent_cancellation = deps.agent.token();
    let turn_cancellation: Arc<std::sync::Mutex<Option<CancellationToken>>> =
        Arc::new(std::sync::Mutex::new(None));
    let interrupt_turn = turn_cancellation.clone();
    let command_cancellation: Arc<std::sync::Mutex<Option<CancellationToken>>> =
        Arc::new(std::sync::Mutex::new(None));
    let interrupt_command = command_cancellation.clone();
    state.interrupt_fn = Some(Box::new(move || {
        if let Ok(active) = interrupt_command.lock()
            && let Some(cancellation) = active.as_ref()
        {
            cancellation.cancel();
            return;
        }
        if let Ok(active) = interrupt_turn.lock()
            && let Some(cancellation) = active.as_ref()
        {
            cancellation.cancel();
        }
        agent_cancellation.cancel();
    }));
    state.approvals = deps.approvals.clone();
    if let Some(approvals) = &state.approvals {
        approvals.set_plan_review_available(true);
    }
    let _plan_review_surface = crate::plan_review::ReviewSurfaceGuard(state.approvals.clone());
    if let Some(path) = deps.prompt_history_path.clone() {
        state.set_prompt_history_store(Arc::new(crate::prompt_history::PromptHistoryStore::new(
            path,
        )));
    }
    state.context_warn_ratio = deps.context_warn_ratio;
    if let Some(onboarding) = deps.onboarding.clone() {
        state.set_onboarding(onboarding);
        if state.connection_setup_is_active() {
            state.onboarding_notice = deps.startup_notice.as_ref().map(|notice| match notice {
                StartupNotice::Command(message)
                | StartupNotice::Info(message)
                | StartupNotice::Error(message)
                | StartupNotice::CommandResult { message, .. } => message.clone(),
            });
        }
    }
    let has_secret_prompt = deps.secret_prompt.is_some();
    let mut secret_rx = match deps.secret_prompt.clone() {
        Some(secret_prompt) => {
            let receiver = secret_prompt.subscribe();
            state.set_secret_prompt(secret_prompt);
            receiver
        }
        None => {
            let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            receiver
        }
    };

    #[cfg(unix)]
    let mut session_host_shutdown = session_background::watch(&deps.agent)?;
    let mut terminal_events = crossterm::event::EventStream::new();
    let mut return_recap = crate::session_control::ReturnRecap::default();
    let voice_owner = deps
        .agent
        .session()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .id()
        .to_string();
    let mut voice = crate::voice::VoiceController::new(
        voice_owner.clone(),
        deps.agent
            .workspace_service()
            .and_then(|owner| owner.shell().subprocess())
            .unwrap_or_else(|| deps.subprocess.clone()),
        workspace.clone(),
    )
    .with_workspace(deps.agent.workspace_service());
    if let Err(error) = optional_questions::refresh(&mut state, &deps) {
        state.items.push(Item::Error(error.to_string()));
    }
    let mut optional_question_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    optional_question_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut optional_question_refresh_error = None;
    let mut busy_tick = tokio::time::interval(std::time::Duration::from_millis(120));
    busy_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    busy_tick.tick().await; // consume the immediate first tick
    let mut task_tick = tokio::time::interval(std::time::Duration::from_millis(120));
    task_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut task_operations = crate::task_console::TaskOperations::default();
    let mut companion_tick = tokio::time::interval(std::time::Duration::from_millis(120));
    companion_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    companion_tick.tick().await;
    let mut pet_tick = tokio::time::interval(std::time::Duration::from_millis(1_200));
    pet_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    pet_tick.tick().await; // consume the immediate first tick
    let mut turn_task: Option<tokio::task::JoinHandle<anyhow::Result<TuiTurnResult>>> = None;
    // Keep automatic native inbox starts behind foreground relay settlement.
    let mut inbox_wake_barrier: Option<heycode_agent::InboxWakeBarrier> = None;
    let mut command_task: Option<(tokio::task::JoinHandle<anyhow::Result<()>>, CommandTiming)> =
        None;
    let mut export_task: Option<(
        tokio::task::JoinHandle<
            Result<
                crate::export_panel::PlainTextExportReceipt,
                crate::export_panel::ConversationExportError,
            >,
        >,
        CancellationToken,
    )> = None;
    let mut authorization_task: Option<AuthorizationOperation> = None;
    let mut assistant_connection: Option<AssistantConnectionOperation> = None;
    let mut assistant_catalog: Option<(String, heycode_llm::CatalogSnapshot)> = None;
    let mut provider_connection: Option<ProviderConnectionOperation> = None;
    let mut provider_catalog: Option<ProviderConnectionCatalog> = None;
    let mut authorization_provider: Option<String> = None;
    let doctor_cancellation = CancellationToken::new();
    let _doctor_cancellation_guard = doctor_cancellation.clone().drop_guard();
    let mut doctor_task = deps.doctor.clone().map(|doctor| {
        let cancellation = doctor_cancellation.clone();
        tokio::spawn(async move { doctor.run(cancellation).await })
    });
    let model_picker_lifecycle = CancellationToken::new();
    let _model_picker_guard = model_picker_lifecycle.clone().drop_guard();
    let mut model_refresh_task: Option<tokio::task::JoinHandle<ModelRefreshOutcome>> = None;
    let mut model_refresh_cancellation: Option<CancellationToken> = None;
    let mut model_effort_task: Option<tokio::task::JoinHandle<ModelEffortOutcome>> = None;
    let mut model_effort_cancellation: Option<CancellationToken> = None;
    let mut model_configuration_operation = if live_model_configuration_applied || !app_opened {
        None
    } else {
        active_configuration.as_ref().and_then(|active| {
            ModelConfigurationOperation::start(
                deps.routing.clone(),
                active,
                &model_picker_lifecycle,
            )
        })
    };
    let mut workspace_context_cancellation = CancellationToken::new();
    let mut workspace_context_task = Some(spawn_workspace_context_probe(
        deps.agent
            .workspace_service()
            .and_then(|owner| owner.shell().subprocess())
            .unwrap_or_else(|| deps.subprocess.clone()),
        workspace.clone(),
        workspace_context_cancellation.clone(),
    ));
    let mut workspace_context_refresh = tokio::time::interval(std::time::Duration::from_secs(30));
    workspace_context_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    workspace_context_refresh.tick().await;

    let result = async {
        loop {
            if let Some(outcome) = state.take_run_outcome() {
                break Ok(outcome);
            }
        if let Some(outcome) = state.add_directory_dialog.as_mut().and_then(|dialog| dialog.poll()) {
            state.add_directory_dialog = None;
            match outcome {
                crate::add_directory::AddDirectoryOutcome::Cancelled => {},
                crate::add_directory::AddDirectoryOutcome::Granted { path, .. } => {
                    state.items.push(Item::Info(format!("Added {} to this session's allowed directories.", crate::markdown::terminal_safe_span(&path.display().to_string()))));
                }
            }
        }
        if !state.high_priority_modal_open()
            && state.pending_plan_review.is_none()
            && state.pending_mcp_elicitation.is_none()
            && let Some(dialog) = add_directory_bridge.as_ref().and_then(|bridge| bridge.take())
        {
            state.close_low_priority_surfaces();
            state.add_directory_dialog = Some(dialog);
        }
        let effective_workspace = deps.agent.cwd();
        if state.native_inbox_available() && workspace != effective_workspace {
            workspace = effective_workspace;
            state.cwd = workspace.clone();
            if let Some(welcome) = &mut state.welcome { welcome.workspace = workspace.clone(); }
            state.set_workspace_context(crate::workspace_context::WorkspaceContextState::Loading);
            workspace_context_cancellation.cancel();
            if let Some(task) = workspace_context_task.take() { task.abort(); let _ = task.await; }
            workspace_context_cancellation = CancellationToken::new();
            let scoped_subprocess = deps.agent.workspace_service().and_then(|owner| owner.shell().subprocess()).unwrap_or_else(|| deps.subprocess.clone());
            workspace_context_task = Some(spawn_workspace_context_probe(scoped_subprocess.clone(), workspace.clone(), workspace_context_cancellation.clone()));
            workspace_context_refresh.reset();
            // Active dictation pins authority, so a committed change reaches
            // this boundary only after its process has settled.
            voice.shutdown().await;
            voice = crate::voice::VoiceController::new(voice_owner.clone(), scoped_subprocess, workspace.clone()).with_workspace(deps.agent.workspace_service());
            if let Some((query, _)) = session_support.as_ref()
                && let Ok(metadata) = heycode_session::SessionCreationMetadata::new(Some(workspace.clone()), Some("native".to_owned()), heycode_session::SessionSource::Interactive)
            {
                state.set_session_service(query.clone(), deps.agent.session().clone(), metadata);
            }
        }
        voice.sync_owner(&mut state);
        if session_events.take_lagged() {
            let session = deps
                .agent
                .session()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            session_rx = session_events.subscribe();
            state.replay(session.events());
        }
        for (key, text) in task_operations.dispatch(&mut state.task_console) {
            state.restore_failed_task_message(key, text);
        }
        if let Some(request) = surfaces.recomposition.take()? {
            if state.recomposition_blocked() {
                state.items.push(Item::Error("Restart deferred: finish the active dialog, draft or queued work, then retry the command.".to_owned()));
            } else if let Some(session_id) = state.current_session_id() {
                request.permit.begin_shutdown();
                break Ok(TuiRunOutcome::RecomposeCurrent { session_id, action: request.action });
            } else {
                state.items.push(Item::Error("Restart requires a durable current session.".to_owned()));
            }
        }
        state.poll_human_command();
        state.poll_agent_readiness();
        state.refresh_workflows();
        if let Some((id, answer)) = state.take_mcp_elicitation_response()
            && mcp_bridge
                .as_ref()
                .is_none_or(|bridge| !bridge.answer(id, answer))
        {
            state.items.push(Item::Error(
                "MCP elicitation was no longer pending".to_owned(),
            ));
        }
        if let Some(files) = state.pending_delivery_save.take() {
            match deps.attachments.clone() {
                Some(store) => {
                    let result = tokio::task::spawn_blocking(move || crate::file_delivery::save_files(&store, &files, CancellationToken::new())).await;
                    match result {
                        Ok(Ok(paths)) => {
                            for path in paths { state.items.push(Item::Info(format!("Saved local file to {}", crate::markdown::terminal_safe_span(&path.display().to_string())))); }
                        }
                        Ok(Err(error)) => state.items.push(Item::Error(format!("Local file save failed: {error}"))),
                        Err(error) => state.items.push(Item::Error(format!("Local file save could not settle: {error}"))),
                    }
                }
                None => state.items.push(Item::Error("Stored attachments are unavailable in this profile".to_owned())),
            }
        }
        if let Some(request) = state.pending_plain_text_export.take() {
            if export_task.is_some() {
                state.items.push(Item::Error("Another conversation export is still running".to_owned()));
            } else if let Some(owner) = deps.agent.workspace_service() {
                let filesystem = owner.filesystem();
                let cwd = state.cwd.clone();
                let cancellation = CancellationToken::new();
                let run_cancellation = cancellation.clone();
                state.close_low_priority_surfaces();
                state.export_progress = Some(crate::export_panel::PlainTextExportProgress::new(&request, cancellation.clone()));
                export_task = Some((tokio::spawn(async move { crate::export_panel::commit_plain_text_export(&filesystem, &cwd, request, run_cancellation).await }), cancellation));
            } else {
                state.items.push(Item::Error("Conversation export filesystem is unavailable".to_owned()));
            }
        }
        let copy_recovery = state.pending_copy_recovery.take();
        let is_copy_command = copy_recovery.as_ref().is_some_and(|(_, clipboard)| *clipboard);
        let mut copy_recovery_path = None;
        if let Some((selection, clipboard)) = copy_recovery {
            let filename = selection.filename().to_owned();
            let written = tokio::task::spawn_blocking(move || selection.write_recovery()).await;
            match written {
                Ok(Ok(path)) => {
                    let path = crate::markdown::terminal_safe_span(&path.display().to_string()).into_owned();
                    if clipboard {
                        copy_recovery_path = Some(path);
                    } else {
                        state.items.push(Item::Info(format!("Written to {path}")));
                        state.show_copy_notice(format!("Written {filename}"));
                    }
                }
                Ok(Err(error)) => state.items.push(Item::Error(error.to_string())),
                Err(error) => state.items.push(Item::Error(format!("Copy file write could not settle: {error}"))),
            }
        }
        if let Some(text) = state.take_clipboard_request() {
            let is_export_command = std::mem::take(&mut state.export_clipboard);
            let mut clipboard_succeeded = false;
            let notice = match crate::terminal::copy_to_clipboard(&deps.subprocess, &mut std::io::stdout(), state.chrome(), &text).await {
                Ok(crate::terminal::ClipboardOutcome::Emitted) => {
                    clipboard_succeeded = true;
                    let lines = text.lines().count().max(1);
                    format!("Copied to clipboard ({} characters, {lines} {})", text.chars().count(), if lines == 1 { "line" } else { "lines" })
                },
                Ok(crate::terminal::ClipboardOutcome::UnsupportedPresentation) => "Copy unavailable in flat output".to_owned(),
                Ok(crate::terminal::ClipboardOutcome::TooLarge) => "Selection is too large to copy".to_owned(),
                Err(_) => "Copy failed".to_owned(),
            };
            if is_export_command {
                if clipboard_succeeded { state.items.push(Item::Info("Conversation copied to clipboard".to_owned())); }
                else { state.items.push(Item::Error(notice)); }
            } else if is_copy_command {
                let message = copy_recovery_path.map_or_else(|| notice.clone(), |path| format!("{notice}\nAlso written to {path}"));
                state.items.push(Item::Info(message));
                state.copy_notice = None;
            } else {
                state.show_copy_notice(notice);
            }
        }
        if let Some((request_id, decision)) = state.take_runtime_permission_response()
            && let Err(error) = app_client.respond_permission(&request_id, decision).await
        {
            state.apply(&UiEvent::Error {
                message: error.to_string(),
            });
        }
        optional_questions::resolve_pending(&mut state, &deps);
        if let Some((request_id, answer)) = state.take_runtime_question_response() {
            let response = if let Some((owner, id)) = parse_tui_agent_question_id(&request_id) {
                let answer = answer.unwrap_or(heycode_agent::QuestionAnswer::Cancelled);
                if owner == question_owner && deps.questions.answer_owned(owner, id, answer) {
                    Ok(())
                } else {
                    Err(heycode_app_server::AppServerError::classified(
                        heycode_app_server::AppServerErrorCode::Conflict,
                    ))
                }
            } else {
                match answer {
                    Some(heycode_agent::QuestionAnswer::Selected(labels)) => app_client.respond_question_selected(&request_id,&labels).await,
                    Some(heycode_agent::QuestionAnswer::Answer(answer)) => app_client.respond_question(&request_id,&answer).await,
                    Some(heycode_agent::QuestionAnswer::Cancelled) => app_client.cancel_question(&request_id).await,
                    None => app_client.cancel_question(&request_id).await,
                }
            };
            if let Err(error) = response {
                state.apply(&UiEvent::Error {
                    message: error.to_string(),
                });
            }
        }
        while let Some((delivery, text)) = state.take_inbox_submission() {
            match submit_tui_inbox(&deps.agent, delivery, &text) {
                Ok((_effective, wake)) => {
                    let pending = deps.agent.pending_inbox();
                    state.apply(&UiEvent::InboxUpdated {
                        next_turn: pending.next_turn,
                        next_step: pending.next_step,
                        wake,
                    });
                }
                Err(error) => {
                    state.replace_input(text);
                    state.apply(&UiEvent::Error {
                        message: root_cause(&error),
                    });
                }
            }
        }
        if let Some(panel) = state.take_panel_open_request() {
            state.open_capability_panel(panel);
        }
        if state.workspace_trust().is_none()
            && !state.onboarding.as_ref().is_some_and(|onboarding| onboarding.active)
            && let Some(request) = session_support
            .as_ref()
            .and_then(|(_, commands)| commands.take())
        {
            state.handle_session_command(request);
            // A session command that asks for recomposition must act now:
            // waiting for the next terminal event would swallow that keystroke
            // into the old world and show nothing until it arrived.
            if let Some(outcome) = state.take_run_outcome() {
                break Ok(outcome);
            }
        }
        if let Some(mode) = state.take_sandbox_selection() {
            if state.runtime != "native" {
                state.items.push(Item::Error("Sandbox changes require a native session.".to_owned()));
            } else if state.recomposition_blocked() {
                state.items.push(Item::Error("Sandbox unchanged: finish the active dialog, draft or queued work, then retry.".to_owned()));
            } else {
                match surfaces.recomposition.request_action(
                    &deps.agent,
                    crate::recomposition::RecompositionAction::Sandbox(mode),
                ).await {
                    Ok(()) => continue,
                    Err(error) => state.items.push(Item::Error(format!("Sandbox unchanged: {error}"))),
                }
            }
        }
        if state.take_model_refresh_cancel() {
            if let Some(cancellation) = model_effort_cancellation.take() {
                cancellation.cancel();
            }
            if let Some(task) = model_effort_task.take() {
                task.abort();
            }
            if let Some(cancellation) = model_refresh_cancellation.take() {
                cancellation.cancel();
            }
            if let Some(task) = model_refresh_task.take() {
                task.abort();
            }
        }
        if let Some((owner, mode)) = state.take_model_refresh_request() {
            if let Some(cancellation) = model_refresh_cancellation.take() {
                cancellation.cancel();
            }
            if let Some(task) = model_refresh_task.take() {
                task.abort();
            }
            let routing = deps.routing.clone();
            let task_owner = owner.clone();
            let cancellation = model_picker_lifecycle.child_token();
            model_refresh_cancellation = Some(cancellation.clone());
            model_refresh_task = Some(tokio::spawn(async move {
                let result = routing
                    .models_owned(&task_owner, mode, cancellation)
                    .await
                    .map_err(|error| error.to_string());
                (task_owner, result)
            }));
        }
        if let Some((owner, revision, model)) = state.take_model_effort_request() {
            if let Some(cancellation) = model_effort_cancellation.take() {
                cancellation.cancel();
            }
            if let Some(task) = model_effort_task.take() {
                task.abort();
            }
            let routing = deps.routing.clone();
            let cancellation = model_picker_lifecycle.child_token();
            model_effort_cancellation = Some(cancellation.clone());
            model_effort_task = Some(tokio::spawn(async move {
                let result = routing.model_effort_catalog_owned(&owner, revision, &model, cancellation).await.map_err(|error| error.to_string());
                (owner, revision, model, result)
            }));
        }
        if let Some(ModelPickerSelection { owner, revision, catalog, model, scope, effort }) = state.take_model_selection() {
            match deps
                .routing
                .select_model_configuration_owned(
                    &owner,
                    revision,
                    Some(catalog.as_ref()),
                    heycode_routing::ModelControlChoice { model: &model, effort: effort.as_deref(), scope },
                    CancellationToken::new(),
                )
                .await
            {
                Ok(_) => match deps.routing.active_configuration() {
                    Ok(active) => {
                        state.apply_active_routing_configuration(&active);
                        state.items.push(Item::Info(match scope {
                            heycode_routing::SelectionScope::Default => format!("Model set to {model} as the default"),
                            heycode_routing::SelectionScope::Session => format!("Model set to {model} for this session"),
                        }));
                        if let Some(selected) =
                            catalog.models.iter().find(|row| row.id == model)
                        {
                            state.apply_model_descriptor(selected);
                        }
                        if let Some(operation) = model_configuration_operation.take() {
                            operation.cancel();
                        }
                        model_configuration_operation = ModelConfigurationOperation::start(
                            deps.routing.clone(),
                            &active,
                            &model_picker_lifecycle,
                        );
                    }
                    Err(error) => state.apply(&UiEvent::Error {
                        message: error.to_string(),
                    }),
                },
                Err(error) => state.apply(&UiEvent::Error {
                    message: error.to_string(),
                }),
            }
        }
        if let Some((owner, revision, effort, scope)) = state.take_effort_selection() {
            match deps
                .routing
                .select_effort_owned_in_scope(
                    &owner,
                    revision,
                    &effort,
                    scope,
                    CancellationToken::new(),
                )
                .await
            {
                Ok(_) => {
                    match deps.routing.active_configuration() {
                        Ok(active) => state.apply_active_routing_configuration(&active),
                        Err(error) => state.apply(&UiEvent::Error {
                            message: error.to_string(),
                        }),
                    }
                    state
                        .items
                        .push(Item::Info(match scope {
                            heycode_routing::SelectionScope::Default => format!("reasoning effort persisted as {effort}"),
                            heycode_routing::SelectionScope::Session => format!("reasoning effort set to {effort} for this session"),
                        }));
                }
                Err(error) => state.apply(&UiEvent::Error {
                    message: error.to_string(),
                }),
            }
        }
        if let Some((current_provider, current_runtime)) = state.take_route_picker_request() {
            match deps.runtimes.descriptors() {
                Ok(runtimes) => state.apply_route_catalog(build_route_rows(
                    &deps.agent.providers().profiles(),
                    &runtimes,
                    &current_provider,
                    &current_runtime,
                )),
                Err(error) => state.apply_route_catalog_error(error.to_string()),
            }
        }
        if let Some(selection) = state.take_route_selection() {
            match selection {
                RoutePickerSelection::Inference {
                    provider,
                    default_model: _,
                } => match deps.routing.select_provider(&provider) {
                    Ok(selection) => {
                        if let Ok(active) = deps.routing.active_configuration() {
                            state.apply_active_routing_configuration(&active);
                        }
                        state.items.push(Item::Info(format!(
                            "native provider persisted as {} with default model {}",
                            selection.provider(),
                            selection.model()
                        )));
                    }
                    Err(error) => state.apply(&UiEvent::Error {
                        message: error.to_string(),
                    }),
                },
                RoutePickerSelection::NativeRuntime { runtime } => {
                    state.update_welcome_runtime(runtime.clone());
                    state
                        .items
                        .push(Item::Info(format!("runtime {runtime} is already active")));
                }
                RoutePickerSelection::DelegatedRuntime { runtime } => {
                    match deps.routing.select_runtime(&runtime) {
                        Ok(selection) => state.items.push(Item::Info(format!(
                            "runtime {} persisted; restart heycode to activate it",
                            selection.runtime()
                        ))),
                        Err(error) => state.apply(&UiEvent::Error {
                            message: error.to_string(),
                        }),
                    }
                }
            }
        }
        if assistant_connection.is_some() && !state.onboarding.as_ref().is_some_and(|view|
            view.active && view.step == heycode_onboarding::OnboardingStep::Assistant)
            && let Some(operation) = assistant_connection.as_ref() {
                operation.cancellation.cancel();
        }
        if !state.onboarding.as_ref().is_some_and(|view| view.active && matches!(view.step, heycode_onboarding::OnboardingStep::AuthorizationMethod | heycode_onboarding::OnboardingStep::Connection | heycode_onboarding::OnboardingStep::Reconnect | heycode_onboarding::OnboardingStep::Endpoint | heycode_onboarding::OnboardingStep::Parameters)) {
            if let Some(operation) = provider_connection.as_ref() { operation.cancellation.cancel(); }
            if let Some(operation) = authorization_task.as_ref() { operation.cancellation.cancel(); }
        }
        if let Some(outcome) = state.onboarding_outcome.take() {
            match outcome {
                heycode_onboarding::OnboardingOutcome::RuntimeClassSelected(class) => {
                    if class == heycode_onboarding::RuntimeClass::Subscription {
                        if let Some(onboarding) = deps.onboarding.as_ref() {
                            let options = deps.runtimes.descriptors()?.into_iter()
                                .filter(|row| row.kind() == heycode_runtime::AgentRuntimeKind::Delegated)
                                .map(|row| heycode_onboarding::OnboardingOption {
                                    id: row.id().as_str().to_owned(),
                                    label: row.display_name().to_owned(),
                                    description: "Check the account signed in through this assistant".into(),
                                }).collect();
                            onboarding.show_assistants(options)?;
                            state.onboarding_notice = None;
                            state.set_onboarding(onboarding.clone());
                        }
                        continue;
                    }
                    if let Some(onboarding) = deps.onboarding.as_ref() {
                        assistant_catalog = None;
                        provider_catalog = None;
                        authorization_provider = None;
                        let local = class == heycode_onboarding::RuntimeClass::Local;
                        let options = deps.routing.connection_profiles().iter()
                            .filter(|profile| (profile.family == heycode_llm::ConnectionFamily::Local) == local)
                            .map(|profile| heycode_onboarding::OnboardingOption {
                                id: profile.registry_name.clone(),
                                label: profile.descriptor.display_name.clone(),
                                description: profile.default_endpoint.clone().unwrap_or_else(|| {
                                    if profile.family == heycode_llm::ConnectionFamily::Local {
                                        "Enter a server URL and choose a model".into()
                                    } else {
                                        "Connect your account and choose a model".into()
                                    }
                                }),
                            }).collect();
                        onboarding.show_connections(local, options)?;
                        state.onboarding_notice = None;
                        state.set_onboarding(onboarding.clone());
                    }
                }
                heycode_onboarding::OnboardingOutcome::ConnectionSelected(provider) => {
                    if provider_connection.is_none() && authorization_task.is_none() {
                        let profile = deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == provider)
                            .ok_or_else(|| anyhow::anyhow!("selected connection is unavailable"))?;
                        provider_catalog = None;
                        if !profile.parameters.is_empty() {
                            if let Some(onboarding) = deps.onboarding.as_ref() {
                                let selection = deps.routing.selection()?;
                                let saved = (selection.provider() == provider)
                                    .then(|| selection.parameters());
                                let fields = profile.parameters.iter().map(|parameter| heycode_onboarding::OnboardingParameter {
                                    id: parameter.id.clone(),
                                    label: parameter.label.clone(),
                                    description: parameter.description.clone(),
                                    value: saved.and_then(|values| values.get(&parameter.id)).cloned().unwrap_or_default(),
                                }).collect();
                                let credential = match deps.models.as_ref() {
                                    Some(models) if models.supports_parameter_credentials(&provider)? => {
                                        heycode_onboarding::OnboardingParameterCredential::Masked
                                    }
                                    Some(_) | None => {
                                        heycode_onboarding::OnboardingParameterCredential::Unavailable
                                    }
                                };
                                onboarding.show_parameters(&provider, credential, fields)?;
                                state.set_onboarding(onboarding.clone());
                                state.onboarding_notice = profile.help.clone();
                            }
                            continue;
                        }
                        if profile.family == heycode_llm::ConnectionFamily::Local {
                            if let Some(onboarding) = deps.onboarding.as_ref() {
                                let selection = deps.routing.selection()?;
                                let endpoint = if selection.provider() == provider { selection.endpoint() } else { None }
                                    .or(profile.default_endpoint.as_deref()).unwrap_or("");
                                onboarding.show_endpoint(&provider, endpoint)?;
                                state.set_onboarding(onboarding.clone());
                                state.onboarding_notice = profile.help.clone();
                            }
                            continue;
                        }
                        let mut validation = None;
                        if let Some(reference) = profile.credential_reference.as_deref() {
                            let Some(authorization) = deps.authorization.as_ref() else {
                                state.onboarding_notice = Some("This profile does not include account authorization.".into());
                                continue;
                            };
                            let flow = authorization.descriptors()?.into_iter().find(|flow| flow.query.reference.as_str() == reference);
                            let Some(flow) = flow else {
                                state.onboarding_notice = Some("This provider needs an authorization plugin before it can connect.".into());
                                continue;
                            };
                            if !authorization.credential_state(&flow.query)?.configured || state.onboarding.as_ref().is_some_and(|view| view.step == heycode_onboarding::OnboardingStep::Reconnect) {
                                state.onboarding_outcome = Some(heycode_onboarding::OnboardingOutcome::AuthorizationFlowSelected(flow.id.as_str().into()));
                                continue;
                            }
                            if flow.method == heycode_authorization::AuthorizationMethod::ApiKey {
                                validation = Some((authorization.clone(), flow.id));
                            }
                        }
                        if let Some(models) = deps.models.as_ref() {
                            provider_connection = Some(ProviderConnectionOperation::start(provider, models.clone(), None, false, validation));
                            state.onboarding_notice = Some("Loading available models…".into());
                        } else { state.onboarding_notice = Some("This profile does not include model discovery.".into()); }
                    }
                },
                heycode_onboarding::OnboardingOutcome::EndpointSelected { provider, endpoint, authenticate } => {
                    if provider_connection.is_none() && let Some(models) = deps.models.as_ref() {
                        provider_catalog = None;
                        provider_connection = if authenticate {
                            if let (Some(authorization), Some(prompt)) = (deps.authorization.as_ref(), deps.secret_prompt.as_ref()) {
                                Some(ProviderConnectionOperation::start_authenticated(provider, endpoint, models.clone(), authorization.clone(), prompt.clone())?)
                            } else {
                                state.onboarding_notice = Some("This profile does not include masked credential entry.".into());
                                continue;
                            }
                        } else { Some(ProviderConnectionOperation::start(provider, models.clone(), Some(endpoint), false, None)) };
                        state.onboarding_notice = Some("Checking the server and available models…".into());
                    }
                }
                heycode_onboarding::OnboardingOutcome::ParametersSelected { provider, parameters, authenticate } => {
                    if provider_connection.is_none() && let Some(models) = deps.models.as_ref() {
                        provider_catalog = None;
                        let profile = deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == provider)
                            .ok_or_else(|| anyhow::anyhow!("selected connection is unavailable"))?;
                        let reference = profile.credential_reference.as_deref()
                            .map(heycode_credentials::CredentialReference::new)
                            .transpose()?;
                        provider_connection = if authenticate {
                            if let (Some(reference), Some(authorization), Some(prompt)) = (reference, deps.authorization.as_ref(), deps.secret_prompt.as_ref()) {
                                Some(ProviderConnectionOperation::start_parameters_authenticated(provider, parameters, reference, models.clone(), authorization.clone(), prompt.clone()))
                            } else {
                                state.onboarding_notice = Some("This cloud profile does not include masked credential entry.".into());
                                continue;
                            }
                        } else {
                            Some(ProviderConnectionOperation::start_parameters(provider, parameters, reference, models.clone()))
                        };
                        state.onboarding_notice = Some("Checking the cloud coordinates and available models…".into());
                    }
                }
                heycode_onboarding::OnboardingOutcome::AssistantSelected(id) => {
                    if assistant_connection.is_none() {
                        let runtime = deps.runtimes.get(&id)?
                            .ok_or_else(|| anyhow::anyhow!("selected assistant is unavailable"))?;
                        let cancellation = CancellationToken::new();
                        let operation_token = cancellation.clone();
                        assistant_catalog = None;
                        let task = tokio::spawn(async move {
                            let account = runtime.account(operation_token.clone()).await?;
                            if !matches!(account.status(), heycode_runtime::AccountStatus::Connected | heycode_runtime::AccountStatus::NotRequired) {
                                return Err(heycode_runtime::RuntimeError::unauthorized());
                            }
                            if runtime.descriptor().capabilities().models.is_supported() {
                                runtime.models(operation_token).await.map(Some)
                            } else {
                                Ok(None)
                            }
                        });
                        assistant_connection = Some(AssistantConnectionOperation { runtime: id, cancellation, task });
                        state.onboarding_notice = Some("Checking your account and available models…".into());
                    }
                }
                heycode_onboarding::OnboardingOutcome::ModelSelected(model) => {
                    if let Some(catalog) = provider_catalog.as_ref() {
                        let candidate = heycode_routing::RoutingSelection::new("native", &catalog.provider, &model, None)
                            .and_then(|selection| selection.with_endpoint(catalog.endpoint.clone()))
                            .and_then(|selection| selection.with_parameters(catalog.parameters.clone()))
                            .map(|selection| selection.with_credential_reference(catalog.credential_reference.clone()));
                        match candidate.and_then(|candidate| deps.routing.stage_connection_selection(&candidate, catalog.snapshot.as_ref())) {
                            Ok(()) => state.run_outcome = Some(TuiRunOutcome::RecomposeConnectionSelection),
                            Err(error) => state.onboarding_notice = Some(error.to_string()),
                        }
                    } else if let Some((runtime, catalog)) = assistant_catalog.as_ref() {
                        match deps.routing.select_runtime_model(runtime, &model, catalog) {
                            Ok(_) => state.run_outcome = Some(TuiRunOutcome::RecomposeConnectionSelection),
                            Err(error) => state.onboarding_notice = Some(error.to_string()),
                        }
                    }
                }
                heycode_onboarding::OnboardingOutcome::AuthorizationFlowSelected(flow) => {
                    if authorization_task.is_none()
                        && let Some(authorization) = deps.authorization.as_ref()
                    {
                        let descriptor = authorization
                            .descriptors()?
                            .into_iter()
                            .find(|descriptor| descriptor.id.as_str() == flow)
                            .ok_or_else(|| {
                                anyhow::anyhow!("authorization flow `{flow}` vanished")
                            })?;
                        authorization_provider = deps.routing.connection_profiles().iter()
                            .find(|profile| profile.credential_reference.as_deref() == Some(descriptor.query.reference.as_str()))
                            .map(|profile| profile.registry_name.clone());
                        let authorization = authorization.clone();
                        let cancellation = CancellationToken::new();
                        let task_cancellation = cancellation.clone();
                        let task = tokio::spawn(async move {
                            authorization
                                .authorize(&descriptor.id, descriptor.query, task_cancellation)
                                .await
                        });
                        authorization_task = Some(AuthorizationOperation { cancellation, task });
                        state.onboarding_notice = Some("Waiting for masked credential input…".to_owned());
                    }
                }
                heycode_onboarding::OnboardingOutcome::ReadyToRecompose => {
                    state.run_outcome = Some(TuiRunOutcome::RecomposeConnection);
                }
                // Cancelling the in-session wizard settles the `/login` echo
                // with a receipt, the way every other command closes.
                heycode_onboarding::OnboardingOutcome::Dismissed => {
                    state.items.push(Item::Info("Login interrupted".to_owned()));
                }
                heycode_onboarding::OnboardingOutcome::None
                | heycode_onboarding::OnboardingOutcome::Exit => {}
            }
        }
        if let Some(outcome) = state.take_run_outcome() { break Ok(outcome); }
        if state.workspace_trust().is_none()
            && turn_task.is_none()
            && command_task.is_none()
            && !state.has_active_turn()
            && state.native_inbox_available()
            && !deps.agent.has_inbox_driver()
            && state.take_follow_up_wake()
        {
            let agent = deps.agent.clone();
            let cancellation = CancellationToken::new();
            if let Ok(mut active) = turn_cancellation.lock() {
                *active = Some(cancellation.clone());
            }
            app_turn_active.store(false, std::sync::atomic::Ordering::SeqCst);
            state.mark_turn_scheduled();
            turn_task = Some(tokio::spawn(run_native_follow_up(agent, cancellation)));
            busy_tick.reset();
        }
        if state.workspace_trust().is_none() && turn_task.is_none() && command_task.is_none() && !state.has_active_turn() {
            state.promote_next_queued_command();
        }
        draw_fn(&mut state);

        if state.quit_requested {
            break Ok(TuiRunOutcome::Exit);
        }

        if let Some(text) = state.pending_send.take() {
            if state.workspace_trust().is_some() || state.connection_setup_is_active() {
                state.replace_input(text);
                continue;
            }
            if let Some((name, args)) = parse_slash(&text) {
                #[cfg(unix)]
                if let Some(result) = session_background::route_domain(&deps.agent, &name, &args) {
                    if let Err(error) = result { state.items.push(Item::Error(error.to_string())); }
                    continue;
                }
                if name == "questions" && args.trim().is_empty() {
                    if let Err(error) = optional_questions::refresh(&mut state, &deps) { state.items.push(Item::Error(error.to_string())); }
                    state.open_optional_questions();
                    continue;
                }
                if name == "voice" {
                    voice.command(&args, &mut state);
                    continue;
                }
                // `/agents` is retained conversation history; the provider and
                // preset catalog is an explicit `/agents providers` request.
                if state.route_task_browser_command(&name, &args) {
                    continue;
                }
                match deps.commands.get(&name) {
                    Ok(Some(cmd)) => {
                        if matches!(cmd.descriptor().id(), "btw" | "recap" | "output-style" | "rewind" | "questions" | "answer")
                            && !state.native_inbox_available()
                        {
                            state.apply(&UiEvent::Error { message: format!("/{name} is available for native provider sessions; the active delegated runtime owns its conversation") });
                            continue;
                        }
                        let availability = cmd.availability();
                        if !availability.is_available() {
                            state.apply(&UiEvent::Error {
                                message: format!(
                                    "/{name} unavailable: {}",
                                    availability.reason().unwrap_or("unknown prerequisite")
                                ),
                            });
                            continue;
                        }
                        let timing = cmd.descriptor().timing();
                        if turn_task.is_some() || state.has_active_turn() {
                            match route_command(timing, true) {
                                CommandDisposition::Queue => {
                                    state.queue_command(text, cmd.descriptor().synopsis());
                                    continue;
                                }
                                CommandDisposition::ConfirmInterrupt => {
                                    state.request_command_confirmation(
                                        text,
                                        cmd.descriptor().synopsis(),
                                    );
                                    continue;
                                }
                                CommandDisposition::ExecuteNow => {}
                            }
                        }
                        if command_task.is_some() {
                            state.queue_command(text, cmd.descriptor().synopsis());
                            continue;
                        }
                        if timing == CommandTiming::ModelScheduling {
                            state.mark_turn_scheduled();
                        }
                        let cancellation = CancellationToken::new();
                        if name == "compact" || (name == "recap" && args.trim().is_empty()) {
                            if name == "compact" {
                                state.begin_compact_command(&text);
                            } else {
                                state.begin_side_command(&text);
                            }
                            if let Ok(mut active) = command_cancellation.lock() {
                                *active = Some(cancellation.clone());
                            }
                        }
                        let agent = deps.agent.clone();
                        command_task = Some((
                            tokio::spawn(async move { cmd.execute_cancellable(&agent, &args, cancellation).await }),
                            timing,
                        ));
                    }
                    Ok(None) => {
                        state.items.push(Item::Notice(unknown_command_notice(&name)));
                        state.scroll_from_bottom = 0;
                    }
                    Err(error) => state.apply(&UiEvent::Error {
                        message: error.to_string(),
                    }),
                }
                continue;
            }
            if turn_task.is_some() || command_task.is_some() || state.has_active_turn() {
                state.replace_input(text);
                state.apply(&UiEvent::Info {
                    text: "busy — wait for active work or use a slash command".to_owned(),
                });
                continue;
            }
            let client = app_client.clone();
            let attachments = state.pending_attachments.clone();
            state.mark_turn_scheduled();
            let cancellation = CancellationToken::new();
            if let Ok(mut active) = turn_cancellation.lock() {
                *active = Some(cancellation.clone());
            }
            inbox_wake_barrier = Some(deps.agent.defer_inbox_wakes());
            app_turn_active.store(true, std::sync::atomic::Ordering::SeqCst);
            let app_ui = ui_tx.clone();
            turn_task = Some(tokio::spawn(async move {
                run_app_server_turn(client, text, attachments, cancellation, app_ui)
                    .await
                    .map(TuiTurnResult::AppServer)
            }));
            busy_tick.reset();
        }

            tokio::select! {
            _ = async {
                #[cfg(unix)]
                if let Some(receiver) = session_host_shutdown.as_mut() { receiver.recv().await; return; }
                futures::future::pending::<()>().await;
            } => { state.quit_requested = true; }
            event = voice.rx.recv() => {
                if let Some(event) = event { voice.event(event, &mut state); }
            }
            maybe_event = terminal_events.next() => {
                match maybe_event {
                    Some(Ok(ev)) => {
                        match ev {
                            crossterm::event::Event::FocusLost => return_recap.lost_focus(),
                            crossterm::event::Event::FocusGained if state.native_inbox_available()
                                && !state.has_active_turn() && command_task.is_none() && state.pending_send.is_none() => {
                                let (completed, latest) = {
                                    let session = deps.agent.session().lock().unwrap_or_else(|e| e.into_inner());
                                    let ends = session.events().iter().filter(|event| matches!(event.kind, heycode_session::SessionEventKind::TurnEnd { .. })).collect::<Vec<_>>();
                                    (ends.len(), ends.last().and_then(|event| u64::try_from(event.time_ms).ok().map(|time| (event.seq, time))))
                                };
                                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|time| u64::try_from(time.as_millis()).ok()).unwrap_or(0);
                                if return_recap.returned(now_ms, completed, latest, deps.agent.automatic_recap_enabled().unwrap_or(false)) {
                                    state.pending_send = Some("/recap".to_owned());
                                }
                            },
                            _ => {},
                        }
                        if state.handle_terminal_event(&ev) {
                            let outcome = state.take_run_outcome().unwrap_or(TuiRunOutcome::Exit);
                            break Ok(outcome);
                        }
                    }
                    Some(Err(err)) => {
                        break Err(anyhow::anyhow!("terminal event error: {err}"));
                    }
                    None => {
                        break Ok(TuiRunOutcome::Exit);
                    }
                }
            }
            maybe_ui = ui_rx.recv() => {
                match maybe_ui {
                    Some(ui) if !session_owned_ui_event(&ui) => state.apply(&ui),
                    Some(_) => {}
                    None => {
                        break Ok(TuiRunOutcome::Exit);
                    }
                }
            }
            maybe_question = question_rx.recv() => {
                match maybe_question {
                    Some(question) if deps.questions.is_pending(question.id) => {
                        state.apply(&UiEvent::RuntimeQuestionRequested {
                            request_id: tui_agent_question_id(question_owner, question.id),
                            mode:question.mode, progress:question.progress,
                            header: question.header,
                            prompt: question.prompt,
                            choices: question
                                .choices
                                .iter()
                                .map(|choice| choice.label.clone())
                                .collect(),
                            choice_descriptions: question
                                .choices
                                .into_iter()
                                .map(|choice| choice.description)
                                .collect(),
                        });
                    }
                    Some(_) => {}
                    None => break Err(anyhow::anyhow!("interactive question surface closed")),
                }
            }
            maybe_session = session_rx.recv() => {
                if let Some(event) = maybe_session {
                    state.apply_session_event(&event);
                }
            }
            maybe_mcp = async {
                match mcp_rx.as_mut() {
                    Some(receiver) => Some(receiver.recv().await),
                    None => std::future::pending().await,
                }
            }, if mcp_rx.is_some() => {
                match maybe_mcp {
                    Some(Ok(event)) => state.receive_mcp_event(event),
                    Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                        state.items.push(Item::Error(
                            "MCP human events were dropped after UI lag".to_owned(),
                        ));
                    }
                    Some(Err(tokio::sync::broadcast::error::RecvError::Closed)) | None => {
                        mcp_rx = None;
                    }
                }
            }
            maybe_secret = secret_rx.recv(), if has_secret_prompt => {
                if let Some(event) = maybe_secret {
                    state.apply_secret_prompt(&event);
                }
            }
            _ = async {
                if let Some((_, until)) = state.copy_notice.as_ref() {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(*until)).await;
                } else { futures::future::pending::<()>().await; }
            }, if state.copy_notice.is_some() => { state.copy_notice = None; }
            _ = optional_question_tick.tick() => {
                match optional_questions::refresh(&mut state, &deps) {
                    Ok(()) => optional_question_refresh_error = None,
                    Err(error) => {
                        let message = error.to_string();
                        if optional_question_refresh_error.as_ref() != Some(&message) { state.items.push(Item::Error(message.clone())); }
                        optional_question_refresh_error = Some(message);
                    }
                }
            }
            _ = task_tick.tick(), if state.add_directory_dialog.as_ref().is_some_and(|dialog| dialog.running()) || state.task_console.attached() || state.workflow_console.attached() || state.agent_readiness.as_ref().is_some_and(crate::agent_readiness::AgentReadinessProbes::active) => { state.refresh_tasks(); state.poll_agent_readiness(); state.refresh_workflows(); if state.workflow_console.runs.iter().any(|r| r.status.active()) { state.spinner = state.spinner.wrapping_add(1); } }
            _ = busy_tick.tick(), if state.chrome().animation() && (state.verb.is_some() || state.side_command_activity.is_some()) && (turn_task.is_some() || command_task.is_some()) => {
                state.tick_spinner();
                state.tick_pet();
            }
            _ = companion_tick.tick(), if state.chrome().animation() && state.shell_preferences().header_pet() && state.companion.is_animating() => {
                state.companion.advance();
            }
            _ = pet_tick.tick(), if state.chrome().animation() && state.shell_preferences().header_pet() && !state.has_active_turn() => {
                state.tick_pet();
            }
            _ = workspace_context_refresh.tick(), if workspace_context_task.is_none() => {
                workspace_context_task = Some(spawn_workspace_context_probe(
                    deps.agent.workspace_service().and_then(|owner| owner.shell().subprocess()).unwrap_or_else(|| deps.subprocess.clone()),
                    workspace.clone(),
                    workspace_context_cancellation.clone(),
                ));
            }
            joined = async {
                match turn_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => {
                        let never: Option<
                            Result<
                                anyhow::Result<TuiTurnResult>,
                                tokio::task::JoinError,
                            >,
                        > = std::future::pending().await;
                        never
                    }
                }
            }, if turn_task.is_some() => {
                turn_task = None;
                app_turn_active.store(false, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut active) = turn_cancellation.lock() {
                    *active = None;
                }
                state.settle_joined_turn(&mut ui_rx, || deps.agent.token().is_turn_active());
                match joined {
                    Some(Ok(Ok(report))) => {
                        if report.is_error() && state.turn_error == TurnErrorRow::None {
                            state.apply(&UiEvent::TurnFinished { reason: "error".into(), usage: None, context_tokens: None });
                        }
                    }
                    Some(Ok(Err(err))) => {
                        // A streamed cause is more useful than the operation's generic wrapper.
                        if state.turn_error != TurnErrorRow::Specific {
                            state.apply(&UiEvent::Error { message: root_cause(&err) });
                        }
                    },
                    Some(Err(join_err)) => {
                        state.apply(&UiEvent::Error { message: join_err.to_string() });
                    }
                    None => {}
                }
                // Direct native lifecycle delivery is enabled and old-turn
                // settlement is complete before the next inbox turn may start.
                drop(inbox_wake_barrier.take());
                if workspace_context_task.is_none() {
                    workspace_context_task = Some(spawn_workspace_context_probe(
                        deps.agent.workspace_service().and_then(|owner| owner.shell().subprocess()).unwrap_or_else(|| deps.subprocess.clone()),
                        workspace.clone(),
                        workspace_context_cancellation.clone(),
                    ));
                    workspace_context_refresh.reset();
                }
            }
            joined = async {
                match export_task.as_mut() {
                    Some((task, _)) => Some(task.await),
                    None => std::future::pending().await,
                }
            }, if export_task.is_some() => {
                export_task = None;
                state.export_progress = None;
                match joined {
                    Some(Ok(Ok(receipt))) => state.items.push(Item::Info(format!("Conversation exported to: {} ({} bytes)", safe_path_display(receipt.path()), receipt.byte_len()))),
                    Some(Ok(Err(error))) => state.items.push(Item::Error(format!("Failed to export conversation: {error}"))),
                    Some(Err(error)) => state.items.push(Item::Error(format!("Conversation export could not settle: {error}"))),
                    None => {}
                }
            }
            joined = async {
                match command_task.as_mut() {
                    Some((task, _)) => Some(task.await),
                    None => {
                        let never: Option<Result<anyhow::Result<()>, tokio::task::JoinError>> =
                            std::future::pending().await;
                        never
                    }
                }
            }, if command_task.is_some() => {
                let timing = command_task.as_ref().map(|(_, timing)| *timing);
                command_task = None;
                let cancelled = command_cancellation.lock().ok()
                    .and_then(|mut active| active.take())
                    .is_some_and(|token| token.is_cancelled());
                state.active_command_draft = None;
                state.side_command_activity = None;
                if timing == Some(CommandTiming::ModelScheduling) {
                    state.mark_turn_settled();
                }
                match joined {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(_))) if cancelled => {}
                    Some(Ok(Err(error))) => state.apply(&UiEvent::Error {
                        message: root_cause(&error),
                    }),
                    Some(Err(error)) => state.apply(&UiEvent::Error {
                        message: error.to_string(),
                    }),
                    None => {}
                }
                // Re-read the routing-owned active plane after every command.
                // The composed Agent selection is only the native fallback
                // while a delegated runtime owns the session.
                if let Ok(active) = deps.routing.active_configuration() {
                    state.apply_active_routing_configuration(&active);
                    if model_configuration_operation
                        .as_ref()
                        .is_some_and(|operation| !operation.matches(&active))
                        && let Some(operation) = model_configuration_operation.take()
                    {
                        operation.cancel();
                    }
                    if model_configuration_operation.is_none()
                        && state.model_display_name.is_none()
                    {
                        model_configuration_operation = ModelConfigurationOperation::start(
                            deps.routing.clone(),
                            &active,
                            &model_picker_lifecycle,
                        );
                    }
                }
                state.update_welcome_permission(deps.agent.approval_kind().as_str().to_owned());
            }
            joined = async {
                match assistant_connection.as_mut() {
                    Some(operation) => Some((&mut operation.task).await),
                    None => std::future::pending().await,
                }
            }, if assistant_connection.is_some() => {
                if let Some(operation) = assistant_connection.take()
                    && !operation.cancellation.is_cancelled() {
                    match joined {
                        Some(Ok(Ok(Some(catalog)))) => {
                            if let Some(onboarding) = deps.onboarding.as_ref() {
                                let options = catalog.models.iter()
                                    .filter(|row| row.lifecycle.is_selectable(unix_time_ms()))
                                    .map(|row| heycode_onboarding::OnboardingOption {
                                        id: row.id.clone(), label: row.display_name.clone(),
                                        description: row.id.clone(),
                                    }).collect::<Vec<_>>();
                                if options.is_empty() {
                                    state.onboarding_notice = Some("This account returned no selectable models. Check the assistant's account and retry.".into());
                                } else {
                                    onboarding.show_models(options)?;
                                    state.set_onboarding(onboarding.clone());
                                    state.onboarding_notice = None;
                                    assistant_catalog = Some((operation.runtime, catalog));
                                }
                            }
                        }
                        Some(Ok(Ok(None))) => match deps.routing.select_runtime(&operation.runtime) {
                            Ok(_) => state.run_outcome = Some(TuiRunOutcome::RecomposeConnectionSelection),
                            Err(error) => state.onboarding_notice = Some(error.to_string()),
                        },
                        Some(Ok(Err(error))) => {
                            let help = deps.runtimes.get(&operation.runtime)?.and_then(|runtime| runtime.descriptor().connection_help().map(str::to_owned));
                            state.onboarding_notice = Some(format!("{error} {}", help.as_deref().unwrap_or("Press Enter to retry, or Esc to choose another connection.")));
                        },
                        Some(Err(_)) => state.onboarding_notice = Some("The connection check stopped. Press Enter to retry.".into()),
                        None => {}
                    }
                }
            }
            joined = async {
                match provider_connection.as_mut() {
                    Some(operation) => Some((&mut operation.task).await),
                    None => std::future::pending().await,
                }
            }, if provider_connection.is_some() => {
                if let Some(operation) = provider_connection.take()
                    && !operation.cancellation.is_cancelled()
                    && let Some(onboarding) = deps.onboarding.as_ref() {
                    if let Some(endpoint) = operation.endpoint.as_deref()
                        && onboarding.snapshot()?.search.as_deref() != Some(endpoint) {
                        state.onboarding_notice = Some("The server address changed. Select Find models to check it.".into());
                        continue;
                    }
                    if !operation.parameters.is_empty()
                        && onboarding.parameter_draft()?.as_ref().is_none_or(|draft| draft.provider != operation.provider || draft.parameters != operation.parameters)
                    {
                        state.onboarding_notice = Some("The cloud coordinates changed. Select Find models to check them.".into());
                        continue;
                    }
                    let profile = deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == operation.provider);
                    let allows_explicit = profile.is_some_and(heycode_llm::ConnectionProfile::allows_explicit_model);
                    if operation.endpoint.is_some()
                        && let Some(Ok(Err(error))) = joined.as_ref()
                        && !(allows_explicit && catalog_allows_explicit_model(error))
                    {
                        provider_catalog = None;
                        state.onboarding_notice = Some(format!("{error}. Check the server address or use an API key to retry."));
                        continue;
                    }
                    if joined.as_ref().is_some_and(|joined| joined.as_ref().is_ok_and(catalog_credential_rejected)) {
                        provider_catalog = None;
                        if !operation.parameters.is_empty() {
                            let supports_masked = deps.models.as_ref()
                                .is_some_and(|models| models.supports_parameter_credentials(&operation.provider).unwrap_or(false));
                            state.onboarding_notice = Some(if supports_masked {
                                "The credential was rejected for these cloud coordinates. Select Use a credential to retry with masked entry.".into()
                            } else {
                                let help = deps.routing.connection_profiles().iter()
                                    .find(|profile| profile.registry_name == operation.provider)
                                    .and_then(|profile| profile.help.as_deref())
                                    .unwrap_or("Configure this cloud account outside heycode, then select Find models to retry.");
                                format!("The cloud account could not authorize these coordinates. {help}")
                            });
                            continue;
                        }
                        let profile = deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == operation.provider)
                            .ok_or_else(|| anyhow::anyhow!("selected connection is unavailable"))?;
                        onboarding.begin_reconnect(heycode_onboarding::OnboardingOption {
                            id: operation.provider.clone(),
                            label: format!("Reconnect {}", profile.descriptor.display_name),
                            description: "The service rejected the current credential. Enter a valid key to retry.".into(),
                        })?;
                        state.onboarding_notice = Some("The API key is invalid or has been revoked. Enter a replacement key.".into());
                        state.set_onboarding(onboarding.clone());
                        if let Some(authorization) = deps.authorization.as_ref()
                            && let Some(flow) = authorization.descriptors()?.into_iter().find(|flow| Some(flow.query.reference.as_str()) == profile.credential_reference.as_deref())
                        {
                            state.onboarding_outcome = Some(heycode_onboarding::OnboardingOutcome::AuthorizationFlowSelected(flow.id.as_str().into()));
                        }
                        continue;
                    }
                    // Catalog fallback is allowed only after independent credential validation.
                    if operation.endpoint.is_none() && !operation.credential_verified.load(std::sync::atomic::Ordering::Acquire) {
                        let failed = match &joined {
                            Some(Ok(Ok(view))) => view.warning.is_some(),
                            _ => true,
                        };
                        if failed {
                            provider_catalog = None;
                            state.onboarding_notice = Some(match &joined {
                                Some(Ok(Err(heycode_llm::CatalogError::Refresh { message, .. }))) => format!("{message} Press Esc to choose another connection."),
                                _ => "The connection check failed. Press Enter to retry, or Esc to choose another connection.".into(),
                            });
                            continue;
                        }
                    }
                    let catalog = match joined {
                        Some(Ok(Ok(view))) => {
                            state.onboarding_notice = view.warning.map(|_| "Showing saved models; the live catalog is unavailable.".into());
                            Some((*view.snapshot).clone())
                        }
                        _ => {
                            state.onboarding_notice = Some("The model catalog is unavailable. You can use the provider's default model or go back and retry.".into());
                            None
                        }
                    };
                    let options = catalog.as_ref().map(|catalog| catalog.models.iter()
                        .filter(|row| profile.is_some_and(|profile| profile.admits_discovered_model(row)) && row.lifecycle.is_selectable(unix_time_ms()))
                        .map(|row| heycode_onboarding::OnboardingOption {
                            id: row.id.clone(), label: row.display_name.clone(), description: row.id.clone(),
                        }).collect::<Vec<_>>()).unwrap_or_default();
                    let options = if options.is_empty() {
                        deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == operation.provider)
                            .and_then(|profile| profile.default_model.as_ref()).map(|model| vec![heycode_onboarding::OnboardingOption {
                                id: model.clone(), label: model.clone(),
                                description: "Provider default; availability has not been checked".into(),
                            }]).unwrap_or_default()
                    } else { options };
                    if options.is_empty() && !allows_explicit {
                        let help = deps.routing.connection_profiles().iter().find(|profile| profile.registry_name == operation.provider).and_then(|profile| profile.help.as_deref()).unwrap_or("Check your connection and retry.");
                        state.onboarding_notice = Some(format!("No selectable models were found. {help}"));
                    } else {
                        if allows_explicit {
                            onboarding.show_models_with_explicit(options)?;
                        } else {
                            onboarding.show_models(options)?;
                        }
                        state.set_onboarding(onboarding.clone());
                        if catalog.is_none() && allows_explicit {
                            state.onboarding_notice = Some("Model discovery is unavailable. Type the exact model ID exposed by this server.".into());
                        }
                        provider_catalog = Some(ProviderConnectionCatalog { provider: operation.provider, endpoint: operation.endpoint, parameters: operation.parameters, credential_reference: operation.credential_reference, snapshot: catalog });
                    }
                }
            }
            joined = async {
                match authorization_task.as_mut() {
                    Some(operation) => Some((&mut operation.task).await),
                    None => {
                        let never: Option<
                            Result<
                                Result<heycode_authorization::AuthorizationReceipt, heycode_authorization::AuthorizationError>,
                                tokio::task::JoinError,
                            >,
                        > = std::future::pending().await;
                        never
                    }
                }
            }, if authorization_task.is_some() => {
                let cancelled = authorization_task.take().is_some_and(|operation| operation.cancellation.is_cancelled());
                if cancelled { authorization_provider = None; continue; }
                match joined {
                    Some(Ok(Ok(receipt))) => {
                        state.onboarding_notice = Some(format!(
                            "Validated and saved via {}.",
                            receipt.committed_by.as_str()
                        ));
                        if let Some(provider) = authorization_provider.take() {
                            if let Some(models) = deps.models.as_ref() {
                                provider_connection = Some(ProviderConnectionOperation::start(provider, models.clone(), None, true, None));
                                state.onboarding_notice = Some("Connected. Loading available models…".into());
                            } else { state.onboarding_notice = Some("Your key is saved, but this profile does not include model discovery.".into()); }
                        } else if let Some(onboarding) = deps.onboarding.as_ref() {
                            onboarding.complete()?;
                            state.set_onboarding(onboarding.clone());
                        }
                    }
                    Some(Ok(Err(error))) => {
                        state.onboarding_notice = Some(format!("{error}. Press Enter to retry, or Esc to go back."));
                    }
                    Some(Err(error)) => {
                        state.onboarding_notice = Some(error.to_string());
                    }
                    None => {}
                }
            }
            joined = async {
                match doctor_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => {
                        let never: Option<
                            Result<Result<heycode_doctor::DoctorReport, heycode_doctor::DoctorError>, tokio::task::JoinError>,
                        > = std::future::pending().await;
                        never
                    }
                }
            }, if doctor_task.is_some() => {
                doctor_task = None;
                match joined {
                    Some(Ok(Ok(report))) => state.apply_doctor_report(&report),
                    Some(Ok(Err(_))) | Some(Err(_)) => {
                        state.set_welcome_health(WelcomeHealth::Unavailable);
                    }
                    None => {}
                }
            }
            joined = async {
                match model_refresh_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => {
                        let never: Option<Result<ModelRefreshOutcome, tokio::task::JoinError>> =
                            std::future::pending().await;
                        never
                    }
                }
            }, if model_refresh_task.is_some() => {
                model_refresh_task = None;
                model_refresh_cancellation = None;
                match joined {
                    Some(Ok((owner, Ok(view)))) => {
                        state.apply_model_catalog_for(&owner, view);
                    }
                    Some(Ok((owner, Err(error)))) => {
                        state.apply_model_catalog_error_for(&owner, error);
                    }
                    Some(Err(error)) if !error.is_cancelled() => {
                        state.apply_model_catalog_error(error.to_string());
                    }
                    Some(Err(_)) | None => {}
                }
            }
            joined = async {
                match model_effort_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => std::future::pending::<Option<Result<ModelEffortOutcome, tokio::task::JoinError>>>().await,
                }
            }, if model_effort_task.is_some() => {
                model_effort_task = None;
                model_effort_cancellation = None;
                if let Some(Ok((owner, revision, model, result))) = joined {
                    state.apply_model_effort_catalog(&owner, revision, &model, result);
                }
            }
            joined = async {
                match model_configuration_operation.as_mut() {
                    Some(operation) => Some((&mut operation.task).await),
                    None => {
                        let never: Option<
                            Result<
                                Result<heycode_runtime::RuntimeModelConfiguration, String>,
                                tokio::task::JoinError,
                            >,
                        > = std::future::pending().await;
                        never
                    }
                }
            }, if model_configuration_operation.is_some() => {
                model_configuration_operation = None;
                if let Some(Ok(Ok(configuration))) = joined {
                    state.apply_discovered_runtime_model_configuration(&configuration);
                }
            }
            joined = async {
                match workspace_context_task.as_mut() {
                    Some(task) => Some(task.await),
                    None => {
                        let never: Option<Result<crate::workspace_context::WorkspaceContextState, tokio::task::JoinError>> =
                            std::future::pending().await;
                        never
                    }
                }
            }, if workspace_context_task.is_some() => {
                workspace_context_task = None;
                match joined {
                    Some(Ok(context)) => state.set_workspace_context(context),
                    Some(Err(error)) if !error.is_cancelled() => state.set_workspace_context(
                        crate::workspace_context::WorkspaceContextState::Unavailable(
                            "workspace lookup stopped",
                        ),
                    ),
                    Some(Err(_)) | None => {}
                }
            }
            }
        }
    }
    .await;

    voice.cancel_for_exit();
    if let Some(operation) = provider_connection.take() {
        let mut task = Some(operation.task);
        cancel_and_join(Some(operation.cancellation), &mut task).await;
    }
    if let Some(operation) = assistant_connection.take() {
        let mut task = Some(operation.task);
        cancel_and_join(Some(operation.cancellation), &mut task).await;
    }
    if let Some(operation) = authorization_task.as_ref() {
        operation.cancellation.cancel();
    }
    let active_turn_cancellation = turn_cancellation
        .lock()
        .ok()
        .and_then(|mut active| active.take());
    if let Some(cancellation) = active_turn_cancellation.as_ref() {
        cancellation.cancel();
    }
    voice.shutdown().await;
    doctor_cancellation.cancel();
    model_picker_lifecycle.cancel();
    workspace_context_cancellation.cancel();
    let model_wait_cancellation = model_refresh_cancellation.take();
    if let Some(cancellation) = model_wait_cancellation.as_ref() {
        cancellation.cancel();
    }
    if let Some((task, timing)) = command_task.as_ref() {
        if let Ok(active) = command_cancellation.lock()
            && let Some(cancellation) = active.as_ref()
        {
            cancellation.cancel();
        }
        if *timing == CommandTiming::ModelScheduling {
            deps.agent.token().cancel();
        } else {
            task.abort();
        }
    }

    if let Some(progress) = state.export_progress.as_mut() {
        progress.cancel();
    }
    if let Some((task, cancellation)) = export_task.take() {
        cancel_and_join(Some(cancellation), &mut Some(task)).await;
    }
    let result = finish_authorization(&mut authorization_task, result).await;
    cancel_and_join(active_turn_cancellation, &mut turn_task).await;
    if let Some((task, _)) = command_task.take() {
        let mut task = Some(task);
        cancel_and_join(None, &mut task).await;
    }
    cancel_and_join(Some(doctor_cancellation), &mut doctor_task).await;
    cancel_and_join(model_wait_cancellation, &mut model_refresh_task).await;
    cancel_and_join(model_effort_cancellation.take(), &mut model_effort_task).await;
    if let Some(operation) = model_configuration_operation.take() {
        let ModelConfigurationOperation {
            cancellation, task, ..
        } = operation;
        let mut task = Some(task);
        cancel_and_join(Some(cancellation), &mut task).await;
    }
    cancel_and_join(
        Some(workspace_context_cancellation),
        &mut workspace_context_task,
    )
    .await;
    let close = if app_opened {
        app_client.close().await
    } else {
        Ok(())
    };
    match (result, close) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(anyhow::anyhow!(error.to_string())),
    }
}

fn method_matches(
    class: heycode_onboarding::RuntimeClass,
    method: heycode_authorization::AuthorizationMethod,
) -> bool {
    use heycode_authorization::AuthorizationMethod as Method;
    use heycode_onboarding::RuntimeClass as Class;
    match class {
        Class::Subscription => {
            matches!(method, Method::OAuth | Method::DeviceCode | Method::Command)
        }
        Class::ApiOrRouter => matches!(method, Method::ApiKey | Method::OAuth),
        Class::Cloud => matches!(method, Method::Ambient | Method::DeviceCode),
        Class::Local => matches!(method, Method::Ambient),
    }
}

fn terminal_safe_human(value: &str, maximum_chars: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\n' || character == '\t' || !character.is_control() {
                character
            } else {
                '\u{fffd}'
            }
        })
        .take(maximum_chars)
        .collect()
}

/// List API connections by provider-owned credential binding, independent of the active route.
#[must_use]
pub fn api_connection_options(
    descriptors: Vec<heycode_authorization::AuthorizationDescriptor>,
    profiles: &[heycode_llm::ConnectionProfile],
) -> Vec<heycode_onboarding::OnboardingOption> {
    descriptors
        .into_iter()
        .filter_map(|descriptor| {
            if descriptor.method != heycode_authorization::AuthorizationMethod::ApiKey {
                return None;
            }
            let profile = profiles.iter().find(|profile| {
                profile.credential_reference.as_deref() == Some(descriptor.query.reference.as_str())
            })?;
            Some(heycode_onboarding::OnboardingOption {
                id: descriptor.id.as_str().to_owned(),
                label: profile.descriptor.display_name.clone(),
                description: descriptor.label,
            })
        })
        .collect()
}

/// Convert the safe flow catalog into runtime-class-specific wizard rows.
#[must_use]
pub fn authorization_options(
    class: heycode_onboarding::RuntimeClass,
    descriptors: Vec<heycode_authorization::AuthorizationDescriptor>,
) -> Vec<heycode_onboarding::OnboardingOption> {
    descriptors
        .into_iter()
        .filter(|descriptor| method_matches(class, descriptor.method))
        .map(|descriptor| heycode_onboarding::OnboardingOption {
            id: descriptor.id.as_str().to_owned(),
            label: descriptor.label,
            description: format!("{:?}", descriptor.method),
        })
        .collect()
}

/// Restrict first-run API-key choices to the provider selected by startup
/// configuration. Other runtime classes retain their contributed method rows.
///
/// Provider switching remains an explicit route/config action; authorization
/// alone never silently changes the active provider.
#[must_use]
pub fn authorization_options_for_provider(
    class: heycode_onboarding::RuntimeClass,
    descriptors: Vec<heycode_authorization::AuthorizationDescriptor>,
    provider: &str,
) -> Vec<heycode_onboarding::OnboardingOption> {
    let mut options = authorization_options(class, descriptors);
    if class == heycode_onboarding::RuntimeClass::ApiOrRouter {
        let flow = format!("{provider}-api-key");
        options.retain(|option| option.id == flow);
    }
    options
}

fn root_cause(err: &anyhow::Error) -> String {
    if let Some(llm_err) = err.downcast_ref::<LlmError>() {
        return llm_err.to_string();
    }
    err.to_string()
}

fn command_transcript_label(name: &str, args: &str) -> String {
    let canonical = format!("/{name}");
    let safe_argument = match name {
        // Session titles are already rendered by the durable rename result.
        "rename"
            if !args.is_empty()
                && args.chars().count() <= 200
                && !args.chars().any(char::is_control) =>
        {
            Some(args)
        }
        // These closed, non-secret vocabularies improve reference fidelity
        // without copying arbitrary command arguments into the transcript.
        "copy"
            if !args.is_empty()
                && args.len() <= 20
                && args.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some(args)
        }
        "color"
            if matches!(
                args,
                "red"
                    | "blue"
                    | "green"
                    | "yellow"
                    | "purple"
                    | "orange"
                    | "pink"
                    | "cyan"
                    | "default"
            ) =>
        {
            Some(args)
        }
        _ => None,
    };
    safe_argument.map_or(canonical.clone(), |argument| {
        format!("{canonical} {argument}")
    })
}

fn safe_path_display(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(char::escape_default)
        .take(512)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod authorization_operation_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn rejected_catalog_requires_repair_even_with_saved_models() {
        use heycode_llm::{
            CatalogError, CatalogFailureKind, CatalogFreshness, CatalogSnapshot, CatalogView,
            ProviderDescriptor,
        };
        let failure = CatalogError::Refresh {
            provider: "openrouter".into(),
            kind: CatalogFailureKind::Unauthorized,
            message: "rejected".into(),
        };
        assert!(catalog_credential_rejected(&Err(failure.clone())));
        let snapshot = Arc::new(CatalogSnapshot {
            provider: ProviderDescriptor {
                id: "openrouter".into(),
                display_name: "OpenRouter".into(),
                protocols: vec![],
            },
            models: vec![],
            revision: 1,
            fetched_at_ms: 1,
        });
        assert!(catalog_credential_rejected(&Ok(CatalogView {
            snapshot,
            freshness: CatalogFreshness::StaleFallback,
            warning: Some(failure)
        })));
        assert!(!catalog_credential_rejected(&Err(CatalogError::Refresh {
            provider: "openrouter".into(),
            kind: CatalogFailureKind::Network,
            message: "offline".into()
        })));
    }

    #[tokio::test]
    async fn settlement_cancels_and_joins_the_authorization_operation() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let settled = Arc::new(AtomicBool::new(false));
        let task_settled = settled.clone();
        let task = tokio::spawn(async move {
            task_cancellation.cancelled().await;
            task_settled.store(true, Ordering::SeqCst);
            Err(heycode_authorization::AuthorizationError::Cancelled)
        });
        let mut operation = Some(AuthorizationOperation { cancellation, task });

        settle_authorization(&mut operation).await;

        assert!(operation.is_none());
        assert!(settled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn error_completion_still_cancels_and_joins_the_authorization_operation() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let settled = Arc::new(AtomicBool::new(false));
        let task_settled = settled.clone();
        let task = tokio::spawn(async move {
            task_cancellation.cancelled().await;
            task_settled.store(true, Ordering::SeqCst);
            Err(heycode_authorization::AuthorizationError::Cancelled)
        });
        let mut operation = Some(AuthorizationOperation { cancellation, task });

        let result: anyhow::Result<()> =
            finish_authorization(&mut operation, Err(anyhow::anyhow!("forced loop failure"))).await;

        assert_eq!(result.unwrap_err().to_string(), "forced loop failure");
        assert!(operation.is_none());
        assert!(settled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn shutdown_bounds_the_wait_on_a_task_that_ignores_cancellation() {
        let cancellation = CancellationToken::new();
        let started = Arc::new(AtomicBool::new(false));
        let task_started = started.clone();
        // A turn task that never observes its token: a provider stream with no
        // deadline behaves exactly like this.
        let mut task = Some(tokio::spawn(async move {
            task_started.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;
        assert!(started.load(Ordering::SeqCst));

        let settle = std::time::Duration::from_millis(50);
        let grace = std::time::Duration::from_millis(10);
        let began = std::time::Instant::now();
        let quit = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            cancel_and_join_within(Some(cancellation), &mut task, settle, grace),
        )
        .await;

        assert!(
            quit.is_ok(),
            "quit must not wait forever on a task that ignores cancellation"
        );
        assert!(
            began.elapsed() < settle * 4,
            "quit waited {:?}",
            began.elapsed()
        );
        assert!(task.is_none());
        // The production path uses the same walk with the shipped bounds.
        assert!(SHUTDOWN_JOIN_TIMEOUT >= settle && SHUTDOWN_ABORT_GRACE >= grace);
    }

    #[tokio::test]
    async fn cancellable_background_task_is_joined_after_cancellation() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let settled = Arc::new(AtomicBool::new(false));
        let task_settled = settled.clone();
        let mut task = Some(tokio::spawn(async move {
            task_cancellation.cancelled().await;
            task_settled.store(true, Ordering::SeqCst);
        }));

        cancel_and_join(Some(cancellation), &mut task).await;

        assert!(task.is_none());
        assert!(settled.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inbox_loop_tests {
    use super::*;
    use heycode_core::{Plugin, compose};
    use heycode_llm::testing::FakeProvider;
    use heycode_llm::{FinishReason, LlmSelection, Provider, StreamChunk};

    #[tokio::test]
    async fn the_loop_follow_up_owner_claims_the_durable_message_and_settles() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![vec![
            StreamChunk::TextDelta("followed".to_owned()),
            StreamChunk::Finish(FinishReason::Stop),
        ]]));
        let plugins: Vec<Box<dyn Plugin>> = vec![
            heycode_session::session_plugin(cwd.clone()),
            heycode_prompt::prompt_plugin(),
            heycode_exec::local_execution_plugin(
                heycode_exec::LocalShellConfig::platform(
                    cwd.clone(),
                    std::time::Duration::from_secs(30),
                )
                .unwrap(),
            ),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
                web_enabled: false,
                ..heycode_tools::ToolsConfig::default()
            }),
            heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
            heycode_llm::token_counters_plugin(),
            heycode_llm::llm_plugin(
                LlmSelection {
                    provider_name: "fake".to_owned(),
                    model: "model-live".to_owned(),
                },
                vec![provider],
            ),
            heycode_agent::approval_plugin(Arc::new(heycode_agent::DenyAll)),
            heycode_agent::agent_options_plugin(heycode_agent::AgentOptions {
                cwd: Some(cwd),
                ..heycode_agent::AgentOptions::default()
            }),
            heycode_agent::commands_plugin(),
            heycode_agent::compactions_plugin(),
            heycode_agent::agent_plugin(),
        ];
        let mut context = compose(&plugins).unwrap();
        let agent = context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap();
        let (_, wake) = agent
            .submit_inbox(heycode_session::InboxDelivery::FollowUp, "inspect next")
            .unwrap();
        assert_eq!(wake, heycode_agent::InboxWake::Wake);

        let result = run_native_follow_up(agent.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(result, TuiTurnResult::NativeFollowUp(_)));
        assert!(agent.pending_inbox().is_empty());
        let session = agent
            .session()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(session.events().iter().any(|event| matches!(
            &event.kind,
            heycode_session::SessionEventKind::UserMessage { text } if text == "inspect next"
        )));
        assert!(session.events().iter().any(|event| matches!(
            event.kind,
            heycode_session::SessionEventKind::TurnEnd {
                reason: heycode_session::TurnEndReason::Stop,
                ..
            }
        )));
        drop(session);
        context.shutdown();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod app_server_event_tests {
    use super::*;

    fn runtime_configuration_event(
        seq: u64,
        state: heycode_session::RuntimeConfigurationState,
        model: &str,
    ) -> heycode_session::SessionEvent {
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms: 1,
            kind: heycode_session::SessionEventKind::RuntimeConfigured {
                state,
                system_prompt: None,
                tools: None,
                model: Some(model.to_owned()),
                reasoning_effort: None,
            },
        }
    }

    #[test]
    fn replay_applies_only_acknowledged_runtime_configuration() {
        let attempted = runtime_configuration_event(
            0,
            heycode_session::RuntimeConfigurationState::Attempted,
            "unacknowledged",
        );
        let failed = runtime_configuration_event(
            1,
            heycode_session::RuntimeConfigurationState::Failed,
            "rejected",
        );
        let committed = runtime_configuration_event(
            2,
            heycode_session::RuntimeConfigurationState::Committed,
            "effective",
        );

        let mut state = AppState::new("original", std::path::PathBuf::from("/workspace"));
        state.replay(&[attempted.clone(), failed.clone()]);
        assert_eq!(state.model, "original");
        state.replay(&[attempted, failed, committed]);
        assert_eq!(state.model, "effective");
    }

    #[test]
    fn runtime_metadata_keeps_the_exact_control_value_and_unknown_context() {
        let mut state = AppState::new("opus[1m]", std::path::PathBuf::from("/workspace"));
        state.runtime = "claude".to_owned();
        state.apply_runtime_model_configuration(
            &heycode_app_server::AppRuntimeModelConfiguration {
                model: "opus[1m]".to_owned(),
                display_name: "Opus (1M context)".to_owned(),
                resolved_model: Some("claude-opus-5[1m]".to_owned()),
                description: Some("Provider supplied description".to_owned()),
                context_window: None,
                default_reasoning_effort: None,
                reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
            },
        );

        assert_eq!(state.model, "opus[1m]", "wire value stays exact");
        assert_eq!(state.active_model_label(), "claude-opus-5[1m]");
        assert_eq!(state.resolved_model.as_deref(), Some("claude-opus-5[1m]"));
        assert_eq!(state.context_window, None, "a label is not a denominator");
        state.apply_runtime_model_configuration(
            &heycode_app_server::AppRuntimeModelConfiguration {
                model: "opus[1m]".to_owned(),
                display_name: "Opus".to_owned(),
                resolved_model: None,
                description: None,
                context_window: None,
                default_reasoning_effort: None,
                reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
            },
        );
        assert_eq!(
            state.active_model_label(),
            "claude-opus-5[1m]",
            "a sparse control refresh cannot erase the reported model"
        );
        state.set_active_model_id("sonnet".to_owned());
        assert_eq!(
            state.active_model_label(),
            "sonnet",
            "a new selector clears prior identity"
        );
    }

    #[test]
    fn advertised_default_effort_is_effective_until_explicitly_overridden() {
        let mut state = AppState::new("runtime-model", std::path::PathBuf::from("/workspace"));
        state.apply_runtime_model_configuration(
            &heycode_app_server::AppRuntimeModelConfiguration {
                model: "runtime-model".to_owned(),
                display_name: "Runtime Model".to_owned(),
                resolved_model: None,
                description: None,
                context_window: Some(200_000),
                default_reasoning_effort: Some("medium".to_owned()),
                reasoning_efforts: vec!["low".to_owned(), "medium".to_owned()],
            },
        );
        assert_eq!(state.reasoning_effort.as_deref(), Some("medium"));
        assert!(state.reasoning_effort_is_default);

        state.set_configured_reasoning_effort(Some("low".to_owned()));
        assert_eq!(state.reasoning_effort.as_deref(), Some("low"));
        assert!(!state.reasoning_effort_is_default);
    }

    #[test]
    fn aggregate_runtime_usage_never_becomes_a_context_measurement() {
        let mut state = AppState::new("runtime-model", std::path::PathBuf::from("/workspace"));
        state.context_window = Some(1_000_000);
        state.apply(&UiEvent::TurnFinished {
            reason: "stop".to_owned(),
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 147_640,
                completion_tokens: 2_000,
            }),
            context_tokens: None,
        });

        assert_eq!(state.context_tokens, None);
        assert!(state.context_meter().is_none());
        assert_eq!(state.usage.unwrap().prompt_tokens, 147_640);
    }

    #[test]
    fn exact_runtime_context_measurement_replaces_the_estimate_and_denominator() {
        let mut state = AppState::new("runtime-model", std::path::PathBuf::from("/workspace"));
        state.context_tokens = Some(99_999);
        state.context_tokens_estimated = true;
        state.context_window = Some(200_000);

        state.apply(&UiEvent::RuntimeContextMeasured {
            resolved_model: Some("claude-opus-5[1m]".to_owned()),
            tokens: 2_767,
            context_window: 1_000_000,
        });

        assert_eq!(state.active_model_label(), "claude-opus-5[1m]");
        assert_eq!(
            state.model, "runtime-model",
            "control selector is unchanged"
        );
        assert_eq!(state.context_tokens, Some(2_767));
        assert_eq!(state.context_window, Some(1_000_000));
        assert!(!state.context_tokens_estimated);
        let meter = state.context_meter().expect("exact context meter");
        assert_eq!(meter.tokens, 2_767);
        assert_eq!(meter.percent, Some(0));
        assert!(!meter.estimated);
    }

    #[test]
    fn stable_app_events_project_to_the_existing_tui_state_plane() {
        let events = app_server_ui_events(heycode_app_server::AppServerEvent::UserInput {
            text: "hello".to_owned(),
            attachments: Vec::new(),
            document_routes: Vec::new(),
        });
        assert!(matches!(events.as_slice(), [UiEvent::UserEcho { text }] if text == "hello"));

        let audio = heycode_core::AttachmentMetadata::new_audio(
            heycode_core::AttachmentContentId::from_sha256([0x74; 32]),
            heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
            16_044,
            None,
            heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
        )
        .unwrap();
        let events = app_server_ui_events(heycode_app_server::AppServerEvent::AssistantAudio {
            attachments: vec![audio.clone()],
        });
        assert!(matches!(
            events.as_slice(),
            [UiEvent::AssistantAudio { attachments }] if attachments == &[audio]
        ));

        let events = app_server_ui_events(heycode_app_server::AppServerEvent::Usage {
            usage: heycode_core::TokenUsage {
                prompt_tokens: 147_640,
                completion_tokens: 2_000,
            },
            context: Some(heycode_app_server::AppRuntimeContextUsage {
                resolved_model: None,
                tokens: 2_767,
                context_window: 1_000_000,
            }),
        });
        assert!(matches!(
            events.as_slice(),
            [UiEvent::RuntimeContextMeasured {
                resolved_model: None,
                tokens: 2_767,
                context_window: 1_000_000,
            }]
        ));

        let events = app_server_ui_events(heycode_app_server::AppServerEvent::ToolFinished {
            call_id: "call-1".to_owned(),
            name: "web_fetch".to_owned(),
            result: serde_json::json!("external"),
            ok: true,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
        });
        assert!(
            events.is_empty(),
            "local tool cards retain richer direct values"
        );

        let events = app_server_ui_events(heycode_app_server::AppServerEvent::TurnFinished {
            turn_id: "1".to_owned(),
            reason: heycode_app_server::AppTurnReason::Cancelled,
            usage: None,
        });
        assert!(matches!(
            events.as_slice(),
            [UiEvent::TurnFinished { reason, .. }] if reason == "aborted"
        ));
        assert!(app_server_owned_ui_event(&UiEvent::AssistantDelta {
            text: "x".to_owned()
        }));
        assert!(!app_server_owned_ui_event(&UiEvent::ConnectRequested));
    }

    #[test]
    fn control_c_interrupts_while_an_approval_card_is_open() {
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = interrupted.clone();
        state.interrupt_fn = Some(Box::new(move || {
            observed.store(true, std::sync::atomic::Ordering::SeqCst)
        }));
        state.apply(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: 0,
            name: "write".into(),
            args_preview: "proof.txt".into(),
        });
        let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(!state.handle_terminal_event(&event));
        assert!(interrupted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            !state
                .items
                .iter()
                .any(|item| matches!(item, Item::Info(text) if text.contains("allowed")))
        );
    }

    #[test]
    fn joined_turn_drains_queued_start_before_settlement_on_transport_failure() {
        let mut state = AppState::new("model", "/workspace".into());
        state.mark_turn_scheduled();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(UiEvent::RuntimeTurnStarted {
                turn_id: "failed-request".into(),
            })
            .unwrap();
        sender
            .send(UiEvent::Error {
                message: "provider disconnected".into(),
            })
            .unwrap();
        // Simulate JoinHandle winning select before the UI receive branch.
        // No TurnFinished is required when the request fails before settlement.
        state.settle_joined_turn(&mut receiver, || false);
        assert!(!state.has_active_turn());
        assert!(state.activity_label().is_none());
        assert!(receiver.try_recv().is_err());
        assert!(
            state
                .items
                .iter()
                .any(|item| matches!(item, Item::Error(text) if text == "provider disconnected"))
        );
        state.mark_turn_scheduled();
        assert!(
            state.has_active_turn(),
            "the next real parent turn must remain active"
        );
    }

    #[test]
    fn joined_turn_preserves_queued_usage_without_replaying_session_owned_text() {
        let mut state = AppState::new("model", "/workspace".into());
        state.items.push(Item::Assistant("old response".into()));
        state.mark_turn_scheduled();
        let boundary = state.activity_item_start;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(UiEvent::TurnStarted { turn: 1 }).unwrap();
        sender
            .send(UiEvent::AssistantDelta {
                text: "new response".into(),
            })
            .unwrap();
        sender
            .send(UiEvent::TurnFinished {
                reason: "completed".into(),
                usage: Some(heycode_core::TokenUsage {
                    prompt_tokens: 321,
                    completion_tokens: 123,
                }),
                context_tokens: None,
            })
            .unwrap();
        state.settle_joined_turn(&mut receiver, || false);
        assert_eq!(state.activity_item_start, boundary);
        assert!(!state.has_active_turn());
        assert!(
            !state
                .items
                .iter()
                .any(|item| matches!(item, Item::Assistant(text) if text.contains("new response"))),
            "session-owned deltas must still arrive through SessionEvent exactly once"
        );
        assert_eq!(state.usage.unwrap().prompt_tokens, 321);
    }

    #[test]
    fn finished_turn_retires_runtime_permission_and_question_cards() {
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        state.apply(&UiEvent::RuntimePermissionRequested {
            request_id: "claude-request-1".into(),
            action: "write".into(),
            detail: "proof.txt".into(),
        });
        assert!(state.pending_ask.is_some());
        state.apply(&UiEvent::TurnFinished {
            usage: None,
            reason: "error".into(),
            context_tokens: None,
        });
        assert!(state.pending_ask.is_none());
        assert!(state.runtime_permission_ids.is_empty());
        assert!(state.queued_asks.is_empty());
        state.resolve_ask_with(heycode_agent::AskAnswer::Allow);
        assert!(state.take_runtime_permission_response().is_none());
    }

    #[test]
    fn stale_local_approval_never_echoes_an_allow_and_withdrawal_removes_queued_cards() {
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        state.approvals = Some(std::sync::Arc::new(
            heycode_agent::InteractiveApproval::new(heycode_core::EventBus::default()),
        ));
        for id in [0, 1] {
            state.apply(&UiEvent::ToolStarted {
                name: "write".into(),
                args: serde_json::json!({"path":"proof.txt"}),
            });
            state.apply(&UiEvent::ApprovalRequested {
                owner_session: None,
                id,
                name: "write".into(),
                args_preview: "path: proof.txt".into(),
            });
        }
        state.resolve_ask_with(heycode_agent::AskAnswer::Allow);
        assert!(
            state
                .items
                .iter()
                .any(|item| matches!(item, Item::Tool { view, .. } if view.approval.as_deref() == Some("cancelled")))
        );
        assert!(
            !state
                .items
                .iter()
                .any(|item| matches!(item, Item::Info(text) if text.contains("✓ allowed")))
        );
        state.apply(&UiEvent::ApprovalResolved {
            id: 1,
            allowed: false,
        });
        assert!(state.pending_ask.is_none());
        assert!(state.queued_asks.is_empty());
    }

    #[test]
    fn a_runtime_permission_request_waits_behind_an_open_tool_dialog() {
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        state.apply(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: 0,
            name: "write".to_owned(),
            args_preview: "path: a".to_owned(),
        });
        state.apply(&UiEvent::RuntimePermissionRequested {
            request_id: "codex-request-1".to_owned(),
            action: "delegated-write".to_owned(),
            detail: "path: b".to_owned(),
        });
        assert_eq!(
            state.pending_ask.as_ref().map(|ask| ask.name.clone()),
            Some("write".to_owned()),
            "the runtime request must not destroy the open tool dialog"
        );

        state.resolve_ask_with(heycode_agent::AskAnswer::Allow);
        assert_eq!(
            state.pending_ask.as_ref().map(|ask| ask.name.clone()),
            Some("delegated-write".to_owned()),
            "the runtime request is asked once the tool dialog is answered"
        );
        state.resolve_ask_with(heycode_agent::AskAnswer::Deny);
        assert!(state.pending_ask.is_none());
        assert_eq!(
            state.take_runtime_permission_response(),
            Some((
                "codex-request-1".to_owned(),
                heycode_app_server::AppPermissionDecision::Deny,
            )),
            "the delegated runtime must hear the denial"
        );
    }

    #[test]
    fn delegated_opaque_turn_and_permission_use_runtime_owned_ui_correlation() {
        let turn = app_server_ui_events(heycode_app_server::AppServerEvent::TurnStarted {
            turn_id: "018f-runtime-turn".to_owned(),
        });
        assert!(matches!(
            turn.as_slice(),
            [UiEvent::RuntimeTurnStarted { turn_id }] if turn_id == "018f-runtime-turn"
        ));

        let permission =
            app_server_ui_events(heycode_app_server::AppServerEvent::PermissionRequested {
                request_id: "codex-request-1".to_owned(),
                action: "Codex command execution".to_owned(),
                detail: "Run tests".to_owned(),
            });
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        state.apply(&permission[0]);
        assert_eq!(
            state.pending_ask.as_ref().unwrap().name,
            "Codex command execution"
        );
        state.resolve_ask_with(heycode_agent::AskAnswer::Allow);
        assert_eq!(
            state.take_runtime_permission_response(),
            Some((
                "codex-request-1".to_owned(),
                heycode_app_server::AppPermissionDecision::AllowOnce,
            ))
        );

        let question =
            app_server_ui_events(heycode_app_server::AppServerEvent::QuestionRequested {
                mode: heycode_core::QuestionMode::SingleChoice,
                progress: (1, 1),
                request_id: "codex-request-2".to_owned(),
                header: Some("Intent".to_owned()),
                prompt: "Continue?".to_owned(),
                choices: vec!["Yes".to_owned(), "No".to_owned()],
                choice_descriptions: vec![
                    Some("Proceed with the work".to_owned()),
                    Some("Stop here".to_owned()),
                ],
            });
        state.apply(&question[0]);
        state.pending_runtime_question.as_mut().unwrap().selection = 1;
        state.resolve_runtime_question();
        assert_eq!(
            state.take_runtime_question_response(),
            Some((
                "codex-request-2".to_owned(),
                Some(heycode_agent::QuestionAnswer::Answer("No".to_owned()))
            ))
        );
    }

    #[test]
    fn question_other_captures_slash_text_and_escape_is_explicit_cancellation() {
        let event = UiEvent::RuntimeQuestionRequested {
            mode: heycode_core::QuestionMode::SingleChoice,
            progress: (1, 1),
            request_id: "agent-question-1".to_owned(),
            header: Some("Intent".to_owned()),
            prompt: "Which route?".to_owned(),
            choices: vec!["A".to_owned(), "B".to_owned()],
            choice_descriptions: vec![
                Some("First route".to_owned()),
                Some("Second route".to_owned()),
            ],
        };
        let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
        state.apply(&event);
        for code in [
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyCode::Enter,
        ] {
            assert!(!state.handle_terminal_event(&crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE,)
            )));
        }
        assert_eq!(
            state.take_runtime_question_response(),
            Some((
                "agent-question-1".to_owned(),
                Some(heycode_agent::QuestionAnswer::Answer("/x".to_owned()))
            ))
        );

        state.apply(&event);
        assert!(!state.handle_terminal_event(&crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            )
        )));
        assert_eq!(
            state.take_runtime_question_response(),
            Some(("agent-question-1".to_owned(), None))
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod thinking_and_budget_tests {
    use super::*;
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn state() -> AppState {
        AppState::new("test-model", std::path::PathBuf::from("/tmp"))
    }

    fn finding_report() -> heycode_session::FindingReport {
        heycode_session::FindingReport::new(
            heycode_session::FindingReportId::new("report-ui").unwrap(),
            heycode_session::FindingReportSource::workspace(
                heycode_core::SessionId::from_raw("session-ui"),
                2,
                0,
                None,
            )
            .unwrap(),
            vec![
                heycode_session::ReportedFinding::new(
                    "finding-ui",
                    heycode_session::ReviewSeverity::High,
                    "src/lib.rs",
                    4,
                    4,
                    "a".repeat(64),
                    "Finding title",
                    "Trigger",
                    "Failure",
                    "Impact",
                )
                .unwrap(),
            ],
        )
        .unwrap()
    }

    #[test]
    fn finding_report_card_toggles_without_becoming_a_model_message() {
        let mut state = state();
        let event = UiEvent::FindingsReported {
            report: finding_report(),
        };
        assert!(session_owned_ui_event(&event));
        state.apply(&event);
        assert!(matches!(
            state.items.as_slice(),
            [Item::FindingsReport {
                expanded: false,
                ..
            }]
        ));
        assert!(!state.items.iter().any(|item| matches!(item, Item::User(_))));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let (row, index) = state.reasoning_hit_rows[0];
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: state.transcript_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: state.transcript_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(
            &state.items[index],
            Item::FindingsReport {
                expanded: true,
                focused: true,
                ..
            }
        ));
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(matches!(
            &state.items[index],
            Item::FindingsReport {
                expanded: false,
                focused: true,
                ..
            }
        ));
        assert!(state.pending_send.is_none());
    }

    #[test]
    fn opaque_and_whitespace_reasoning_have_no_disclosure_control() {
        let mut state = state();
        for text in ["", " ", "\n\t"] {
            state.apply(&UiEvent::ReasoningDelta { text: text.into() });
        }
        state.apply(&UiEvent::AssistantDelta {
            text: "answer".into(),
        });
        assert!(
            !state
                .items
                .iter()
                .any(|item| matches!(item, Item::Reasoning { .. }))
        );
        state.apply(&UiEvent::ReasoningDelta {
            text: "Visible".into(),
        });
        state.apply(&UiEvent::ReasoningDelta { text: "\n".into() });
        state.apply(&UiEvent::ReasoningDelta {
            text: "summary".into(),
        });
        state.apply(&UiEvent::AssistantDelta {
            text: "result".into(),
        });
        let item = state
            .items
            .iter()
            .find(|item| matches!(item, Item::Reasoning { .. }))
            .unwrap();
        let Item::Reasoning { text, view, .. } = item else {
            unreachable!()
        };
        assert_eq!(text, "Visible\nsummary");
        assert!(!view.label(true, false).contains(" 0s"));
    }

    #[test]
    fn brief_thinking_is_hidden_and_keeps_full_text_when_expanded() {
        let mut state = state();
        let text = (0..25)
            .map(|n| format!("reasoning line {n}\n"))
            .collect::<String>();
        state.apply(&UiEvent::ReasoningDelta { text: text.clone() });
        state.apply(&UiEvent::AssistantDelta {
            text: "answer".into(),
        });
        let Item::Reasoning { done, view, .. } = &state.items[0] else {
            panic!("missing reasoning")
        };
        assert!(*done);
        assert_eq!(view.elapsed_seconds, Some(0));
        let lines = crate::render::render_transcript_item(
            &state.items[0],
            crate::render::ItemNeighbors::default(),
            80,
            false,
            state.styles(),
        );
        assert!(
            lines.is_empty(),
            "subsecond completed thinking stays hidden"
        );
        state.toggle_reasoning_item(0);
        let expanded = crate::render::render_transcript_item(
            &state.items[0],
            crate::render::ItemNeighbors::default(),
            80,
            false,
            state.styles(),
        );
        assert!(
            expanded
                .iter()
                .any(|line| line.to_string().contains("reasoning line 0"))
        );
        assert!(
            expanded
                .iter()
                .any(|line| line.to_string().contains("reasoning line 24"))
        );
    }

    #[test]
    fn mouse_and_keyboard_toggle_only_the_selected_block() {
        let mut state = state();
        for text in ["first", "second"] {
            state.apply(&UiEvent::ReasoningDelta { text: text.into() });
            state.apply(&UiEvent::AssistantDelta {
                text: "answer".into(),
            });
        }
        // Mouse disclosure applies to visible blocks; subsecond completed
        // thinking is deliberately omitted from the collapsed transcript.
        for item in &mut state.items {
            if let Item::Reasoning { view, .. } = item {
                view.elapsed_seconds = Some(1);
            }
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let (row, index) = state.reasoning_hit_rows[0];
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: state.transcript_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: state.transcript_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        let Item::Reasoning { view, .. } = &state.items[index] else {
            panic!("not reasoning")
        };
        assert_eq!(view.expanded, Some(true));
        let Item::Reasoning { view, .. } = &state.items[2] else {
            panic!("not reasoning")
        };
        assert_eq!(view.expanded, None);
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let Item::Reasoning { view, .. } = &state.items[index] else {
            panic!("not reasoning")
        };
        assert_eq!(view.expanded, Some(false));
        assert!(state.pending_send.is_none());
    }

    #[test]
    fn tool_start_ends_timer_and_abort_marks_only_open_phase() {
        let mut state = state();
        state.apply(&UiEvent::ReasoningDelta {
            text: "phase one".into(),
        });
        state.apply(&UiEvent::ToolStarted {
            name: "read".into(),
            args: serde_json::json!({}),
        });
        state.apply(&UiEvent::ReasoningDelta {
            text: "phase two".into(),
        });
        state.apply(&UiEvent::TurnFinished {
            reason: "aborted".into(),
            usage: None,
            context_tokens: None,
        });
        let Item::Reasoning { view: first, .. } = &state.items[0] else {
            panic!("missing phase")
        };
        assert!(!first.interrupted);
        let Item::Reasoning {
            view: second, done, ..
        } = &state.items[2]
        else {
            panic!("missing phase")
        };
        assert!(*done && second.interrupted);
    }

    #[test]
    fn copy_notice_is_above_composer_right_aligned_and_expires() {
        let mut state = state();
        state.show_copy_notice("Copied 42 characters".to_owned());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = buffer
            .content
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let notice = rows
            .iter()
            .position(|row| row.contains("Copied 42 characters"))
            .unwrap();
        assert!(rows[notice].ends_with("Copied 42 characters "));
        assert!(rows.iter().skip(notice + 1).any(|row| row.contains('❯')));
        state.copy_notice = Some((
            "Copied 42 characters".into(),
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        ));
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!screen.contains("Copied 42 characters"));
    }

    #[test]
    /// The budget crosses the app-server boundary intact and is dropped the
    /// moment the model it was measured against changes. The narrow idle
    /// footer preserves its estimated context alongside permission controls.
    fn budget_survives_transport_and_remains_visible_in_the_idle_footer() {
        let mut state = state();
        let budget = heycode_llm::context_budget(
            "p".into(),
            "test-model".into(),
            &heycode_llm::EnvelopeTotal::Estimated(250_000),
            Some(1_000_000),
            0,
            8192,
            0.8,
            true,
        );
        for event in
            app_server_ui_events(heycode_app_server::AppServerEvent::ContextBudgetChanged {
                budget: Box::new(budget.clone()),
            })
        {
            state.apply(&event);
        }
        assert_eq!(state.context_budget.as_ref(), Some(&budget));
        state.permission = "ask".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            screen.contains("ctx:~250k/1M (~75% left)"),
            "the idle footer must preserve context size, limit and estimated remaining capacity: {screen}"
        );
        assert!(
            !screen.contains("in:"),
            "unreported billing usage must not be invented: {screen}"
        );
        assert!(screen.contains("manual mode on"), "{screen}");
        state.set_active_model_id("different".into());
        assert!(state.context_budget.is_none());
        assert!(state.context_tokens.is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod session_control_tests {
    use super::*;

    #[test]
    fn composer_submission_resumes_transcript_follow_mode() {
        struct Immediate(heycode_agent::CommandDescriptor);
        #[async_trait::async_trait]
        impl heycode_agent::Command for Immediate {
            fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
                &self.0
            }
            async fn execute(
                &self,
                _agent: &heycode_agent::Agent,
                _args: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let mut commands = CommandRegistry::new();
        commands
            .register(Arc::new(Immediate(
                heycode_agent::CommandDescriptor::new(
                    "latest",
                    "Show the latest result",
                    Vec::new(),
                    heycode_agent::CommandTiming::Immediate,
                    heycode_agent::CommandSource::from_plugin("commands").unwrap(),
                )
                .unwrap(),
            )))
            .unwrap();
        let mut state = AppState::new("test", std::env::temp_dir());
        state.set_commands(Arc::new(commands));
        state.scroll_from_bottom = 20;
        state.replace_input("/latest".to_owned());

        state.submit_composer();

        assert_eq!(state.pending_send.as_deref(), Some("/latest"));
        assert_eq!(state.scroll_from_bottom, 0);
        assert!(matches!(state.items.as_slice(), [Item::Command(text)] if text == "/latest"));
        assert!(!state.focus_view());
        assert!(!state.items.iter().any(|item| matches!(item, Item::User(_))));
    }

    #[test]
    fn command_transcript_labels_expose_only_reviewed_bounded_arguments() {
        assert_eq!(
            command_transcript_label("rename", "Parity   command session"),
            "/rename Parity   command session"
        );
        assert_eq!(command_transcript_label("copy", "2"), "/copy 2");
        assert_eq!(command_transcript_label("color", "cyan"), "/color cyan");
        assert_eq!(
            command_transcript_label("connect", "sk-secret-value"),
            "/connect"
        );
        assert_eq!(
            command_transcript_label("add-dir", "/private/customer"),
            "/add-dir"
        );
        assert_eq!(
            command_transcript_label("rename", &"x".repeat(201)),
            "/rename"
        );
    }

    #[test]
    fn btw_palette_submission_restores_full_draft_and_preserves_active_turn() {
        struct Aside(heycode_agent::CommandDescriptor);
        #[async_trait::async_trait]
        impl heycode_agent::Command for Aside {
            fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
                &self.0
            }
            async fn execute(
                &self,
                _agent: &heycode_agent::Agent,
                _args: &str,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }
        let mut commands = CommandRegistry::new();
        commands
            .register(Arc::new(Aside(
                heycode_agent::CommandDescriptor::new(
                    "btw",
                    "Side question",
                    vec![heycode_agent::CommandArgument::required("question", "Question").unwrap()],
                    heycode_agent::CommandTiming::Immediate,
                    heycode_agent::CommandSource::from_plugin("commands").unwrap(),
                )
                .unwrap(),
            )))
            .unwrap();
        let mut state = AppState::new("test", std::env::temp_dir());
        state.set_commands(Arc::new(commands));
        state.replace_input("unfinished\nmain draft".to_owned());
        state.input.move_cursor(tui_textarea::CursorMove::Head);
        let cursor = state.input.cursor();
        state.active_turn = true;
        state.open_command_palette();
        state.replace_input("/bt".to_owned());
        state.refresh_command_palette();
        state.run_or_complete_palette_command();
        assert_eq!(state.input.lines().join("\n"), "/btw ");
        state.input.insert_str("a quick question");
        state.run_or_complete_palette_command();
        assert_eq!(state.pending_send.as_deref(), Some("/btw a quick question"));
        assert_eq!(state.input.lines().join("\n"), "unfinished\nmain draft");
        assert_eq!(state.input.cursor(), cursor);
        assert!(state.active_turn);
    }
}

#[cfg(test)]
mod memory_panel_routing_tests {
    use super::{AppState, Item};
    use crate::memory_commands::{MemoryAuthority, MemoryManagerError, MemorySourceManager};
    use heycode_prompt::instructions::InstructionSources;
    use std::sync::Arc;

    struct Authority(InstructionSources);
    impl MemoryAuthority for Authority {
        fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn memory_chooser_consumes_input_and_shows_only_the_deliberate_selection() -> anyhow::Result<()>
    {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("AGENTS.md"), "private instruction fixture")?;
        let mut state = AppState::new("test", home.path().to_path_buf());
        state.memory_sources = Some(Arc::new(MemorySourceManager::new(Arc::new(Authority(
            InstructionSources {
                user_home: Some(home.path().to_path_buf()),
                workspace: None,
            },
        )))));
        state.replace_input("preserved draft".to_owned());
        state.apply(&heycode_agent::UiEvent::CapabilityPanelRequested {
            panel: heycode_agent::UiPanelId::new("memory")?,
        });
        assert!(state.memory_panel.is_some());
        assert!(!state.items.iter().any(
            |item| matches!(item, Item::Info(text) if text.contains("private instruction fixture"))
        ));
        assert!(!state.handle_terminal_event(&Event::Paste("/quit\n".to_owned())));
        assert_eq!(state.input.lines(), ["preserved draft"]);
        assert!(!state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))));
        assert!(state.memory_panel.is_none());
        assert_eq!(state.input.lines(), ["preserved draft"]);
        assert!(state.items.iter().any(|item| matches!(item, Item::Info(text) if text.contains("private instruction fixture") && text.contains("revision:"))));

        let item_count = state.items.len();
        state.apply(&heycode_agent::UiEvent::CapabilityPanelRequested {
            panel: heycode_agent::UiPanelId::new("memory")?,
        });
        assert!(state.memory_panel.is_some());
        assert!(
            !state.handle_terminal_event(&Event::Paste(
                "/memory clear user:agents stale".to_owned(),
            ))
        );
        assert!(
            !state.handle_terminal_event(&Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )))
        );
        assert!(state.memory_panel.is_none());
        assert_eq!(state.input.lines(), ["preserved draft"]);
        assert_eq!(state.items.len(), item_count);
        Ok(())
    }
}

#[cfg(test)]
mod copy_panel_integration_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn state() -> AppState {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.replace_input("preserve this draft".to_owned());
        state.items.push(Item::Assistant(
            "A code example:\n```python\nprint(1)\n```\n".to_owned(),
        ));
        state
    }

    #[test]
    fn copy_chooser_isolates_draft_and_separates_copy_write_and_cancel() {
        let mut state = state();
        let cursor = state.input.cursor();
        state.copy_answer(0);
        assert!(state.copy_panel.is_some());
        state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
        assert_eq!(state.input.lines(), ["preserve this draft"]);
        assert_eq!(state.input.cursor(), cursor);
        assert!(state.pending_send.is_none());
        state.handle_terminal_event(&key(KeyCode::Esc));
        assert!(state.copy_panel.is_none());
        assert!(state.pending_copy_recovery.is_none());
        assert!(state.take_clipboard_request().is_none());
        state.copy_answer(0);
        state.handle_terminal_event(&key(KeyCode::Down));
        state.handle_terminal_event(&key(KeyCode::Char('w')));
        assert!(state.take_clipboard_request().is_none());
        let selected = state.pending_copy_recovery.take();
        assert!(selected.is_some_and(|(selection, clipboard)| !clipboard
            && selection.text() == "print(1)"
            && selection.filename() == "copy.python"));
        state.copy_answer(0);
        state.handle_terminal_event(&key(KeyCode::Down));
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert_eq!(state.take_clipboard_request().as_deref(), Some("print(1)"));
        assert!(state.pending_copy_recovery.is_some());
        assert_eq!(state.input.lines(), ["preserve this draft"]);
    }

    #[test]
    fn always_copy_uses_real_settings_and_can_be_reverted() -> anyhow::Result<()> {
        struct Writer;
        impl heycode_settings::SettingsWriter for Writer {
            fn persist_user(
                &self,
                _: &heycode_settings::SettingsNamespace,
                _: &serde_json::Value,
            ) -> Result<(), String> {
                Ok(())
            }
        }
        let settings = Arc::new(heycode_settings::SettingsService::with_writer(
            heycode_settings::SettingsDocuments::new(),
            Arc::new(Writer),
        ));
        let mut context = heycode_core::Context::default();
        settings.register(&context, heycode_ui::preferences::settings_definition()?)?;
        let store = heycode_ui::preferences::SettingsBackedUiPreferences::new(settings.clone());
        let mut state = state();
        state.settings_service = Some(settings);
        state.copy_answer(0);
        state.handle_terminal_event(&key(KeyCode::Down));
        state.handle_terminal_event(&key(KeyCode::Down));
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert!(store.load()?.preferences.copy_full_response());
        state.pending_copy_recovery = None;
        let _ = state.take_clipboard_request();
        state.copy_answer(0);
        assert!(state.copy_panel.is_none());
        assert!(
            state
                .take_clipboard_request()
                .is_some_and(|text| text.starts_with("A code example:"))
        );
        let current = store.load()?;
        store.store(
            &current.preferences.with_copy_full_response(false),
            current.revision,
        )?;
        state.pending_copy_recovery = None;
        state.copy_answer(0);
        assert!(state.copy_panel.is_some());
        context.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod compact_interaction_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    #[test]
    fn recap_progress_renders_without_an_active_parent_turn() -> anyhow::Result<()> {
        let mut state = AppState::new("model", PathBuf::from("/workspace"));
        state.begin_side_command("/recap");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))?;
        terminal.draw(|frame| crate::render::draw(frame, &mut state))?;
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Recapping conversation… (Esc to cancel)"));
        assert!(!state.has_active_turn());
        Ok(())
    }

    #[test]
    fn recap_escape_restores_draft_without_starting_or_settling_parent() {
        for parent_active in [false, true] {
            let mut state = AppState::new("model", PathBuf::from("/workspace"));
            let parent_verb = parent_active.then(|| "Thinking…".to_owned());
            state.replace_input("/recap".to_owned());
            state.input.move_cursor(tui_textarea::CursorMove::Head);
            let cursor = state.input.cursor();
            state.submit_composer();
            if parent_active {
                state.mark_turn_scheduled();
            }
            state.begin_side_command("/recap");
            assert!(!state.is_compacting());
            assert_eq!(state.has_active_turn(), parent_active);
            let token = CancellationToken::new();
            let interrupt = token.clone();
            state.interrupt_fn = Some(Box::new(move || interrupt.cancel()));
            state.handle_terminal_event(&Event::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
            assert!(token.is_cancelled());
            assert_eq!(state.input.lines().join("\n"), "/recap");
            assert_eq!(state.input.cursor(), cursor);
            assert_eq!(state.has_active_turn(), parent_active);
            assert_eq!(state.verb, parent_verb);
        }
    }

    #[test]
    fn provider_status_cannot_replace_compaction_activity() {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.begin_compact_command("/compact keep focus");
        state.apply(&UiEvent::Status {
            verb: "Responding…".to_owned(),
        });
        assert_eq!(
            state.activity_label(),
            Some("Compacting conversation… (Esc to cancel)")
        );
        assert!(state.is_compacting());
    }

    #[test]
    fn compact_escape_restores_exact_ephemeral_command_and_cursor() {
        let mut state = AppState::new("model", PathBuf::from("/workspace"));
        state.replace_input("/compact focus on API choices\nand cancellation".to_owned());
        state.input.move_cursor(tui_textarea::CursorMove::Head);
        state.input.move_cursor(tui_textarea::CursorMove::Forward);
        let text = state.input.lines().join("\n");
        let cursor = state.input.cursor();
        state.submit_composer();
        assert_eq!(state.pending_send.as_deref(), Some(text.as_str()));
        state.mark_turn_scheduled();
        state.begin_compact_command(&text);
        let token = CancellationToken::new();
        let interrupt = token.clone();
        state.interrupt_fn = Some(Box::new(move || interrupt.cancel()));
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(token.is_cancelled());
        assert_eq!(state.input.lines().join("\n"), text);
        assert_eq!(state.input.cursor(), cursor);
        assert!(state.active_command_draft.is_none());
        assert!(state.verb.is_none());
    }

    #[test]
    fn compact_cancel_does_not_overwrite_a_newer_composer_draft() {
        let mut state = AppState::new("model", PathBuf::from("/workspace"));
        state.mark_turn_scheduled();
        state.begin_compact_command("/compact original focus");
        state.replace_input("newer unsent text".to_owned());
        state.restore_interrupted_command_draft();
        assert_eq!(state.input.lines().join("\n"), "newer unsent text");
    }

    #[test]
    fn compact_receipt_disclosure_preserves_summary_and_handles_keyboard() {
        let mut state = AppState::new("model", PathBuf::from("/workspace"));
        state.items.push(Item::Compaction {
            native: false,
            strategy: None,
            replaced_upto_seq: 4,
            summary: Some("exact retained summary".to_owned()),
            provider_items: 0,
            expanded: false,
            focused: false,
        });
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        assert!(
            matches!(state.items.last(), Some(Item::Compaction { expanded: true, focused: false, summary: Some(summary), .. }) if summary == "exact retained summary")
        );
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        assert!(
            matches!(state.items.last(), Some(Item::Compaction { expanded: false, summary: Some(summary), .. }) if summary == "exact retained summary")
        );
        assert!(state.pending_send.is_none());
    }
}

#[cfg(test)]
mod export_integration_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn unsettled_export_consumes_input_and_keeps_cancellation_visible() {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.replace_input("keep draft".to_owned());
        let token = CancellationToken::new();
        let request =
            crate::export_panel::PlainTextExportRequest::new("output.txt", Arc::from("snapshot"))
                .unwrap();
        state.export_progress = Some(crate::export_panel::PlainTextExportProgress::new(
            &request,
            token.clone(),
        ));
        state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
        assert_eq!(state.input.lines(), ["keep draft"]);
        assert!(state.pending_send.is_none());
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(token.is_cancelled());
        assert!(
            state
                .export_progress
                .as_ref()
                .unwrap()
                .plain_lines()
                .iter()
                .any(|line| line.contains("Cancelling"))
        );
        state.open_plain_text_export(None);
        assert!(
            state.export_panel.is_none(),
            "cannot replace an unsettled export"
        );
        assert!(state.export_progress.is_some());
    }

    #[test]
    fn export_snapshot_uses_rendered_conversation_and_excludes_active_command() {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.items.push(Item::User("Earlier question".to_owned()));
        state
            .items
            .push(Item::Assistant("Visible answer with words".to_owned()));
        state.items.push(Item::Command("/export".to_owned()));
        let text = crate::render::conversation_export_text(&state).unwrap();
        assert!(text.contains("Earlier question"), "{text}");
        assert!(text.contains("Visible answer with words"), "{text}");
        assert!(!text.contains("/export"), "{text}");
        assert!(!text.contains('\u{1b}'));
        state.open_plain_text_export(Some("nested/conversation".to_owned()));
        let request = state.pending_plain_text_export.take().unwrap();
        assert_eq!(
            request.destination(),
            std::path::Path::new("nested/conversation.txt")
        );
        assert_eq!(request.transcript(), text);
    }

    #[test]
    fn export_chooser_keeps_snapshot_and_draft_without_paste_execution() {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.replace_input("unfinished\nmain draft".to_owned());
        let cursor = state.input.cursor();
        state
            .items
            .push(Item::Assistant("Original snapshot".to_owned()));
        state.open_plain_text_export(None);
        assert!(state.export_panel.is_some());
        state.items.push(Item::Assistant("Later output".to_owned()));
        state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
        assert!(state.pending_send.is_none());
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(state.export_panel.is_none());
        let copied = state.take_clipboard_request().unwrap();
        assert!(copied.contains("Original snapshot"));
        assert!(!copied.contains("Later output"));
        assert!(state.export_clipboard);
        assert!(state.pending_plain_text_export.is_none());
        assert_eq!(state.input.lines().join("\n"), "unfinished\nmain draft");
        assert_eq!(state.input.cursor(), cursor);
        state.open_plain_text_export(None);
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(state.export_panel.is_none());
        assert!(state.take_clipboard_request().is_none());
        assert!(state.pending_plain_text_export.is_none());
    }
    #[test]
    fn advisor_modal_preserves_draft_and_reports_unavailable_commit() {
        let mut state = AppState::new("test", std::env::temp_dir());
        state.replace_input("unfinished\nmain draft".to_owned());
        let cursor = state.input.cursor();
        let status = heycode_agent::AdvisorStatus {
            selection: None,
            generation: 0,
            settings_revision: 0,
            descendant_budget: heycode_agent::SubagentBudgetSnapshot {
                limits: Default::default(),
                requests_reserved: 0,
                in_flight: 0,
            },
        };
        state
            .advisor_bridge
            .request(crate::advisor_panel::AdvisorPanelView::new(&status, vec![]));
        state.poll_human_command();
        assert!(state.advisor_panel.is_some());
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(
            state.advisor_panel.as_ref().unwrap().notice(),
            Some("Advisor service unavailable")
        );
        state.handle_terminal_event(&Event::Paste("/quit\n".to_owned()));
        assert!(state.pending_send.is_none());
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(state.advisor_panel.is_none());
        assert_eq!(state.input.lines().join("\n"), "unfinished\nmain draft");
        assert_eq!(state.input.cursor(), cursor);
    }
}

#[cfg(test)]
mod model_effort_selection_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(state: &mut AppState, code: KeyCode) {
        state.handle_terminal_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn state() -> AppState {
        let mut state = AppState::new("alpha", std::env::temp_dir());
        let owner = BackendControlOwner::NativeInference {
            provider: "fixture".to_owned(),
        };
        state.open_model_picker(owner, 7, "alpha");
        let models = ["alpha", "beta"]
            .into_iter()
            .map(|id| {
                let mut model = heycode_llm::ModelDescriptor::unknown(id);
                model.lifecycle = heycode_llm::ModelLifecycle::stable();
                model.capabilities.tools = heycode_llm::CapabilitySupport::Supported;
                model
            })
            .collect();
        state.apply_model_catalog(heycode_llm::CatalogView {
            snapshot: Arc::new(heycode_llm::CatalogSnapshot {
                provider: heycode_llm::ProviderDescriptor {
                    id: "fixture".to_owned(),
                    display_name: "Fixture".to_owned(),
                    protocols: vec![heycode_llm::ProviderProtocol::OpenAiChatCompletions],
                },
                models,
                revision: 1,
                fetched_at_ms: 1,
            }),
            freshness: heycode_llm::CatalogFreshness::Live,
            warning: None,
        });
        preview(&mut state);
        state
    }

    fn preview(state: &mut AppState) {
        let (owner, revision, _) = state.take_model_effort_request().unwrap();
        state.model_picker.as_mut().unwrap().effort_picker = Some(EffortPickerView::new(
            owner,
            revision,
            Some("low".to_owned()),
            vec!["low".to_owned(), "high".to_owned()],
            Some("low".to_owned()),
        ));
    }

    #[test]
    fn effort_preview_and_model_commit_share_one_scoped_choice() {
        for (commit_key, expected_scope) in [
            (KeyCode::Enter, heycode_routing::SelectionScope::Default),
            (KeyCode::Char('s'), heycode_routing::SelectionScope::Session),
        ] {
            let mut state = state();
            key(&mut state, KeyCode::Right);
            assert!(state.take_model_selection().is_none());
            key(&mut state, commit_key);
            let ModelPickerSelection {
                owner,
                revision,
                model,
                scope,
                effort,
                ..
            } = state.take_model_selection().unwrap();
            assert_eq!(
                owner,
                BackendControlOwner::NativeInference {
                    provider: "fixture".to_owned()
                }
            );
            assert_eq!(revision, 7);
            assert_eq!(model, "alpha");
            assert_eq!(scope, expected_scope);
            assert_eq!(effort.as_deref(), Some("high"));
            assert!(state.take_model_selection().is_none());
        }
        let mut unchanged = state();
        key(&mut unchanged, KeyCode::Enter);
        assert_eq!(
            unchanged.take_model_selection().unwrap().effort.as_deref(),
            Some("low"),
            "Enter commits the effort shown even without an arrow adjustment"
        );
        let mut state = state();
        key(&mut state, KeyCode::Right);
        key(&mut state, KeyCode::Esc);
        assert!(state.take_model_selection().is_none());
    }

    #[test]
    fn changed_highlight_cannot_commit_or_receive_previous_model_effort() {
        let mut state = state();
        let owner = state.model_picker.as_ref().unwrap().owner.clone();
        key(&mut state, KeyCode::Right);
        key(&mut state, KeyCode::Down);
        state.apply_model_effort_catalog(&owner, 7, "alpha", Err("stale alpha error".to_owned()));
        assert!(state.model_picker.as_ref().unwrap().effort_error.is_none());
        key(&mut state, KeyCode::Enter);
        let ModelPickerSelection { model, effort, .. } = state.take_model_selection().unwrap();
        assert_eq!(model, "beta");
        assert_eq!(effort, None);
    }
}
