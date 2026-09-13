//! Durable human-plane session lifecycle values and local-store helpers.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::{Session, SessionCreationMetadata, SessionEventKind};

pub(crate) const ARCHIVE_MARKER_NAME: &str = ".archived";
pub(crate) const TRASH_DIRECTORY_NAME: &str = ".trash";
pub(crate) const EXPORT_DIRECTORY_NAME: &str = ".exports";
const MAX_SESSION_TITLE_CHARS: usize = 200;
const MAX_MARKDOWN_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXPORT_ANCESTORS: usize = 64;
const MAX_REDACTED_SUPPORT_EVENTS: usize = 10_000;
const MAX_REDACTED_SUPPORT_BYTES: usize = 8 * 1024 * 1024;

/// Validated human-facing session title accepted by new lifecycle writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTitle(String);

impl SessionTitle {
    /// Normalize human input into a bounded, one-line terminal-safe title.
    ///
    /// # Errors
    /// Text with no visible title after normalization is refused.
    pub fn from_input(value: &str) -> Result<Self, crate::SessionQueryError> {
        let safe: String = value
            .chars()
            .filter(|c| c.is_whitespace() || !unsafe_title_character(*c))
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        Self::new(
            safe.trim()
                .chars()
                .take(MAX_SESSION_TITLE_CHARS)
                .collect::<String>(),
        )
    }

    /// Derive a local title from the first nonempty user message, without inference.
    #[must_use]
    pub fn from_conversation(events: &[crate::SessionEvent]) -> Self {
        events
            .iter()
            .find_map(|event| match &event.kind {
                SessionEventKind::UserMessage { text } => {
                    let excerpt = text
                        .split_whitespace()
                        .take(12)
                        .collect::<Vec<_>>()
                        .join(" ");
                    Self::from_input(&excerpt).ok()
                }
                _ => None,
            })
            .unwrap_or_else(|| Self("Untitled session".to_owned()))
    }

    pub(crate) fn with_ordinal(&self, ordinal: usize) -> Self {
        let suffix = format!(" ({ordinal})");
        let prefix: String = self
            .0
            .chars()
            .take(MAX_SESSION_TITLE_CHARS.saturating_sub(suffix.chars().count()))
            .collect();
        Self(format!("{}{suffix}", prefix.trim_end()))
    }

    /// Validate one bounded, trimmed, terminal-safe title.
    ///
    /// # Errors
    /// Blank, overlong, control-bearing or bidi-control text is refused.
    pub fn new(value: impl Into<String>) -> Result<Self, crate::SessionQueryError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.trim() != value
            || value.chars().count() > MAX_SESSION_TITLE_CHARS
            || value.chars().any(unsafe_title_character)
        {
            return Err(crate::SessionQueryError::InvalidQuery);
        }
        Ok(Self(value))
    }

    /// Safe title text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Creation facts and optional initial title for `/new`.
#[derive(Clone)]
pub struct SessionCreateRequest {
    metadata: SessionCreationMetadata,
    title: Option<SessionTitle>,
}

impl SessionCreateRequest {
    /// Create a request from already validated immutable metadata.
    #[must_use]
    pub fn new(metadata: SessionCreationMetadata) -> Self {
        Self {
            metadata,
            title: None,
        }
    }

    /// Commit this title immediately after `session/created`.
    #[must_use]
    pub fn with_title(mut self, title: SessionTitle) -> Self {
        self.title = Some(title);
        self
    }

    pub(crate) fn metadata(&self) -> &SessionCreationMetadata {
        &self.metadata
    }

    pub(crate) fn title(&self) -> Option<&SessionTitle> {
        self.title.as_ref()
    }
}

/// Durable storage visibility of one session directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStorageState {
    /// Visible in the ordinary active-session picker.
    Active,
    /// Recoverably hidden by an owner-controlled marker without moving JSONL.
    Archived,
}

/// Which storage states one query includes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionStorageFilter {
    /// Active sessions only; this is the safe picker default.
    #[default]
    Active,
    /// Archived sessions only.
    Archived,
    /// Both active and archived sessions.
    All,
}

/// Root/fork lineage predicate for session queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionLineageFilter {
    /// Roots and forks.
    #[default]
    All,
    /// Sessions without a parent.
    Roots,
    /// Sessions with verified shared-prefix lineage.
    Forks,
}

/// Archive marker transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionArchiveAction {
    /// Create the recoverable archive marker.
    Archive,
    /// Remove the marker and return to the ordinary picker.
    Restore,
}

/// Delete safety context supplied by a host that owns the current selection.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionDeleteRequest {
    id: heycode_core::SessionId,
    current: Option<heycode_core::SessionId>,
}

impl SessionDeleteRequest {
    /// Target one exact session id.
    #[must_use]
    pub fn new(id: heycode_core::SessionId) -> Self {
        Self { id, current: None }
    }

    /// Protect the host's current session even if no open-handle proof exists.
    #[must_use]
    pub fn with_current(mut self, current: heycode_core::SessionId) -> Self {
        self.current = Some(current);
        self
    }

    pub(crate) fn id(&self) -> &heycode_core::SessionId {
        &self.id
    }

    pub(crate) fn current(&self) -> Option<&heycode_core::SessionId> {
        self.current.as_ref()
    }
}

/// Recoverable-trash receipt returned only after the directory move commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDeleteReceipt {
    session_id: heycode_core::SessionId,
    recovery_id: heycode_core::SessionId,
}

impl SessionDeleteReceipt {
    pub(crate) fn new(
        session_id: heycode_core::SessionId,
        recovery_id: heycode_core::SessionId,
    ) -> Self {
        Self {
            session_id,
            recovery_id,
        }
    }

    /// Deleted session identity.
    #[must_use]
    pub const fn session_id(&self) -> &heycode_core::SessionId {
        &self.session_id
    }

    /// Opaque token needed by the lower service to restore this exact move.
    #[must_use]
    pub const fn recovery_id(&self) -> &heycode_core::SessionId {
        &self.recovery_id
    }
}

/// Lower-service export representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionExportFormat {
    /// Directory bundle containing byte-exact JSONL for the target and every
    /// required ancestor, so a suffix-only fork never masquerades as standalone.
    LosslessJsonl,
    /// Human-readable Markdown projection of the verified logical session.
    Markdown,
    /// Bounded structural trace with no session/provider/host content fields.
    RedactedSupport,
}

/// Committed export artifact owned beneath the session store.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionExportReceipt {
    path: PathBuf,
    format: SessionExportFormat,
    byte_len: u64,
}

impl SessionExportReceipt {
    pub(crate) fn new(path: PathBuf, format: SessionExportFormat, byte_len: u64) -> Self {
        Self {
            path,
            format,
            byte_len,
        }
    }

    /// Committed artifact path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Export representation.
    #[must_use]
    pub const fn format(&self) -> SessionExportFormat {
        self.format
    }

    /// Exact committed JSONL/Markdown bytes, excluding directory metadata.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }
}

impl std::fmt::Debug for SessionExportReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionExportReceipt")
            .field("format", &self.format)
            .field("byte_len", &self.byte_len)
            .finish_non_exhaustive()
    }
}

pub(crate) fn storage_state(
    directory: &Path,
) -> Result<SessionStorageState, crate::SessionQueryError> {
    let marker = directory.join(ARCHIVE_MARKER_NAME);
    match fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(crate::SessionQueryError::InvalidSession)
        }
        Ok(metadata) if metadata.len() != 0 => Err(crate::SessionQueryError::InvalidSession),
        Ok(_) => Ok(SessionStorageState::Archived),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SessionStorageState::Active),
        Err(_) => Err(crate::SessionQueryError::InvalidSession),
    }
}

pub(crate) fn apply_archive_marker(
    directory: &Path,
    action: SessionArchiveAction,
) -> Result<(), crate::SessionQueryError> {
    let marker = directory.join(ARCHIVE_MARKER_NAME);
    match action {
        SessionArchiveAction::Archive => {
            if storage_state(directory)? == SessionStorageState::Archived {
                return Err(crate::SessionQueryError::AlreadyArchived);
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            set_private_file_mode(&mut options);
            let file = options
                .open(&marker)
                .map_err(|_| crate::SessionQueryError::StoreUnavailable)?;
            file.sync_data()
                .map_err(|_| crate::SessionQueryError::StoreUnavailable)?;
            sync_directory(directory)?;
        }
        SessionArchiveAction::Restore => {
            if storage_state(directory)? == SessionStorageState::Active {
                return Err(crate::SessionQueryError::NotArchived);
            }
            fs::remove_file(marker).map_err(|_| crate::SessionQueryError::StoreUnavailable)?;
            sync_directory(directory)?;
        }
    }
    Ok(())
}

pub(crate) fn trash_path(
    root: &Path,
    receipt: &SessionDeleteReceipt,
) -> Result<PathBuf, crate::SessionQueryError> {
    let trash = ensure_private_directory(root, TRASH_DIRECTORY_NAME)?;
    Ok(trash.join(format!(
        "{}--{}",
        receipt.session_id.as_str(),
        receipt.recovery_id.as_str()
    )))
}

pub(crate) fn export_lossless_jsonl(
    root: &Path,
    id: &heycode_core::SessionId,
) -> Result<SessionExportReceipt, crate::SessionQueryError> {
    let sessions = lineage_sessions(root, id)?;
    let exports = ensure_private_directory(root, EXPORT_DIRECTORY_NAME)?;
    let export_id = heycode_core::SessionId::generate();
    let stage = exports.join(format!(".heycode-export-{}.tmp", export_id.as_str()));
    let final_path = exports.join(format!("jsonl-{}", export_id.as_str()));
    fs::create_dir(&stage).map_err(|_| crate::SessionQueryError::ExportFailed)?;
    if let Err(error) = set_private_directory_mode(&stage) {
        let _ = fs::remove_dir(&stage);
        return Err(error);
    }
    let result = (|| {
        let mut byte_len = 0_u64;
        for session in sessions.iter().rev() {
            let directory = stage.join(session.id().as_str());
            fs::create_dir(&directory).map_err(|_| crate::SessionQueryError::ExportFailed)?;
            set_private_directory_mode(&directory)?;
            let source = session
                .validated_raw_suffix()
                .map_err(|_| crate::SessionQueryError::ExportFailed)?;
            byte_len = byte_len
                .checked_add(
                    u64::try_from(source.len())
                        .map_err(|_| crate::SessionQueryError::ExportFailed)?,
                )
                .ok_or(crate::SessionQueryError::ExportFailed)?;
            write_private_file(&directory.join(crate::session::LOG_FILE_NAME), &source)?;
            sync_directory(&directory)?;
        }
        sync_directory(&stage)?;
        fs::rename(&stage, &final_path).map_err(|_| crate::SessionQueryError::ExportFailed)?;
        sync_directory(&exports)?;
        Ok(SessionExportReceipt::new(
            final_path.clone(),
            SessionExportFormat::LosslessJsonl,
            byte_len,
        ))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

pub(crate) fn export_markdown(
    root: &Path,
    id: &heycode_core::SessionId,
) -> Result<SessionExportReceipt, crate::SessionQueryError> {
    let session = Session::open(root.join(id.as_str())).map_err(crate::query::map_open_error)?;
    let exports = ensure_private_directory(root, EXPORT_DIRECTORY_NAME)?;
    let export_id = heycode_core::SessionId::generate();
    let stage = exports.join(format!(".heycode-export-{}.tmp", export_id.as_str()));
    let final_path = exports.join(format!("session-{}-{}.md", id.as_str(), export_id.as_str()));
    let markdown = render_markdown(&session)?;
    write_private_file(&stage, markdown.as_bytes())?;
    fs::rename(&stage, &final_path).map_err(|_| crate::SessionQueryError::ExportFailed)?;
    sync_directory(&exports)?;
    Ok(SessionExportReceipt::new(
        final_path,
        SessionExportFormat::Markdown,
        u64::try_from(markdown.len()).map_err(|_| crate::SessionQueryError::ExportFailed)?,
    ))
}

pub(crate) fn export_redacted_support(
    root: &Path,
    id: &heycode_core::SessionId,
) -> Result<SessionExportReceipt, crate::SessionQueryError> {
    let session = Session::open(root.join(id.as_str())).map_err(crate::query::map_open_error)?;
    let event_count = session.events().len();
    let first_local = usize::try_from(session.first_local_seq())
        .map_err(|_| crate::SessionQueryError::ExportFailed)?;
    let local_event_count = event_count
        .checked_sub(first_local)
        .ok_or(crate::SessionQueryError::ExportFailed)?;
    let omitted = event_count.saturating_sub(MAX_REDACTED_SUPPORT_EVENTS);
    let events = session
        .events()
        .iter()
        .skip(omitted)
        .map(redacted_event)
        .collect::<Vec<_>>();
    let trace = serde_json::json!({
        "schemaVersion":1,
        "redacted":true,
        "eventCount":event_count,
        "localEventCount":local_event_count,
        "omittedPrefixEvents":omitted,
        "lineage":session.lineage().map(|parent| serde_json::json!({
            "fork":true,
            "seedEventCount":parent.seed_event_count()
        })).unwrap_or_else(|| serde_json::json!({"fork":false})),
        "events":events
    });
    let bytes =
        serde_json::to_vec_pretty(&trace).map_err(|_| crate::SessionQueryError::ExportFailed)?;
    if bytes.len() > MAX_REDACTED_SUPPORT_BYTES {
        return Err(crate::SessionQueryError::ExportFailed);
    }
    let exports = ensure_private_directory(root, EXPORT_DIRECTORY_NAME)?;
    let export_id = heycode_core::SessionId::generate();
    let stage = exports.join(format!(".heycode-export-{}.tmp", export_id.as_str()));
    let final_path = exports.join(format!("support-{}.json", export_id.as_str()));
    write_private_file(&stage, &bytes)?;
    if let Err(error) = fs::rename(&stage, &final_path) {
        let _ = fs::remove_file(&stage);
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            crate::SessionQueryError::ExportFailed
        } else {
            crate::SessionQueryError::StoreUnavailable
        });
    }
    sync_directory(&exports)?;
    Ok(SessionExportReceipt::new(
        final_path,
        SessionExportFormat::RedactedSupport,
        u64::try_from(bytes.len()).map_err(|_| crate::SessionQueryError::ExportFailed)?,
    ))
}

fn redacted_event(event: &crate::SessionEvent) -> serde_json::Value {
    let mut data = serde_json::Map::from_iter([
        ("seq".to_owned(), serde_json::json!(event.seq)),
        ("timeMs".to_owned(), serde_json::json!(event.time_ms)),
        ("kind".to_owned(), serde_json::json!(event.kind.name())),
    ]);
    match &event.kind {
        SessionEventKind::TurnStart { turn } => insert_numbers(&mut data, [("turn", *turn)]),
        SessionEventKind::TurnEnd { turn, reason } => {
            insert_numbers(&mut data, [("turn", *turn)]);
            data.insert(
                "reason".to_owned(),
                serde_json::json!(turn_end_reason(*reason)),
            );
        }
        SessionEventKind::StepStart { turn, step } | SessionEventKind::StepEnd { turn, step } => {
            insert_turn_step(&mut data, *turn, *step);
        }
        SessionEventKind::RequestHeader {
            turn, step, header, ..
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert(
                "toolCount".to_owned(),
                serde_json::json!(header.tools.len()),
            );
            data.insert(
                "providerOptionCount".to_owned(),
                serde_json::json!(header.options.provider_options.len()),
            );
            data.insert(
                "nativeToolRouteCount".to_owned(),
                serde_json::json!(header.options.native_tool_routes.len()),
            );
        }
        SessionEventKind::RequestContext { context, .. } => {
            data.insert(
                "contextWindow".to_owned(),
                serde_json::json!(context.context_window),
            );
            data.insert(
                "maxOutputTokens".to_owned(),
                serde_json::json!(context.max_output_tokens),
            );
        }
        SessionEventKind::UserAttachments {
            attachments,
            document_routes,
        } => {
            data.insert(
                "attachmentCount".to_owned(),
                serde_json::json!(attachments.len()),
            );
            data.insert(
                "documentRouteCount".to_owned(),
                serde_json::json!(document_routes.len()),
            );
        }
        SessionEventKind::AttachmentAdded { attachment } => {
            data.insert(
                "byteLength".to_owned(),
                serde_json::json!(attachment.byte_len()),
            );
        }
        SessionEventKind::AssistantAudio {
            turn,
            step,
            attachments,
            ..
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert(
                "attachmentCount".to_owned(),
                serde_json::json!(attachments.len()),
            );
        }
        SessionEventKind::AssistantChunk {
            turn,
            step,
            text,
            reasoning,
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert(
                "textBytes".to_owned(),
                serde_json::json!(text.as_ref().map(String::len)),
            );
            data.insert(
                "reasoningBytes".to_owned(),
                serde_json::json!(reasoning.as_ref().map(String::len)),
            );
        }
        SessionEventKind::AssistantMessage {
            turn,
            step,
            content,
            reasoning,
            tool_calls,
            usage,
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert("contentBytes".to_owned(), serde_json::json!(content.len()));
            data.insert(
                "reasoningBytes".to_owned(),
                serde_json::json!(reasoning.as_ref().map(String::len)),
            );
            data.insert(
                "toolCallCount".to_owned(),
                serde_json::json!(tool_calls.as_ref().map_or(0, Vec::len)),
            );
            if let Some(usage) = usage {
                data.insert("usage".to_owned(), redacted_usage(*usage));
            }
        }
        SessionEventKind::AssistantProviderItem {
            turn,
            step,
            output_index,
            ..
        }
        | SessionEventKind::ServerToolCall {
            turn,
            step,
            output_index,
            ..
        }
        | SessionEventKind::ServerToolResult {
            turn,
            step,
            output_index,
            ..
        }
        | SessionEventKind::AssistantCitation {
            turn,
            step,
            output_index,
            ..
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert("outputIndex".to_owned(), serde_json::json!(output_index));
        }
        SessionEventKind::ServerToolUsage {
            turn, step, usage, ..
        } => {
            insert_turn_step(&mut data, *turn, *step);
            data.insert("requests".to_owned(), serde_json::json!(usage.requests()));
            match usage.cost() {
                heycode_core::ServerToolUsageCost::Unknown => {
                    data.insert("costKnown".to_owned(), serde_json::json!(false));
                }
                heycode_core::ServerToolUsageCost::Published(cost) => {
                    data.insert("costKnown".to_owned(), serde_json::json!(true));
                    match u64::try_from(cost.pico_units()) {
                        Ok(value) => {
                            data.insert("costPicoUnits".to_owned(), serde_json::json!(value));
                        }
                        Err(_) => {
                            data.insert("costOverflow".to_owned(), serde_json::json!(true));
                        }
                    }
                }
            }
        }
        SessionEventKind::AssistantResponseMetadata {
            turn,
            step,
            metadata,
            ..
        } => {
            insert_turn_step(&mut data, *turn, *step);
            if let Some(cache) = metadata.cache_usage() {
                data.insert("cacheUsage".to_owned(), redacted_cache_usage(cache));
            }
            data.insert(
                "contextEdits".to_owned(),
                serde_json::Value::Array(
                    metadata
                        .context_edits()
                        .iter()
                        .map(|edit| {
                            serde_json::json!({
                                "kind":match edit.kind() {
                                    heycode_core::ContextEditKind::ClearThinking => "clear_thinking",
                                    heycode_core::ContextEditKind::ClearToolUses => "clear_tool_uses",
                                },
                                "clearedUnits":edit.cleared_units(),
                                "clearedInputTokens":edit.cleared_input_tokens()
                            })
                        })
                        .collect(),
                ),
            );
            data.insert(
                "cachePrefixImpact".to_owned(),
                serde_json::json!(match metadata.cache_prefix_impact() {
                    Some(heycode_core::CachePrefixImpact::Preserved) => Some("preserved"),
                    Some(heycode_core::CachePrefixImpact::InvalidatedAtEdit) => {
                        Some("invalidated_at_edit")
                    }
                    None => None,
                }),
            );
        }
        SessionEventKind::ToolCall { turn, .. } => {
            insert_numbers(&mut data, [("turn", *turn)]);
        }
        SessionEventKind::ToolResult { is_error, .. }
        | SessionEventKind::RichToolResult { is_error, .. } => {
            data.insert("error".to_owned(), serde_json::json!(is_error));
        }
        SessionEventKind::CompactionApplied {
            summary,
            replaced_upto_seq,
        } => {
            data.insert("summaryBytes".to_owned(), serde_json::json!(summary.len()));
            data.insert(
                "replacedUptoSeq".to_owned(),
                serde_json::json!(replaced_upto_seq),
            );
        }
        SessionEventKind::NativeCompactionApplied {
            replaced_upto_seq,
            items,
            usage,
            ..
        } => {
            data.insert(
                "replacedUptoSeq".to_owned(),
                serde_json::json!(replaced_upto_seq),
            );
            data.insert("itemCount".to_owned(), serde_json::json!(items.len()));
            if let Some(usage) = usage {
                data.insert("usage".to_owned(), redacted_usage(*usage));
            }
        }
        SessionEventKind::PlanReview { decision, .. } => {
            data.insert("decision".to_owned(), serde_json::json!(decision));
        }
        SessionEventKind::PlanMode { active } => {
            data.insert("active".to_owned(), serde_json::json!(active));
        }
        SessionEventKind::AgentInboxSplice {
            start,
            removed_count,
            inserted,
            ..
        } => {
            data.insert("start".to_owned(), serde_json::json!(start));
            data.insert("removedCount".to_owned(), serde_json::json!(removed_count));
            data.insert(
                "insertedCount".to_owned(),
                serde_json::json!(inserted.len()),
            );
        }
        SessionEventKind::HookContribution { contribution } => {
            data.insert(
                "phase".to_owned(),
                serde_json::json!(match contribution.phase() {
                    crate::HookContributionPhase::Pre => "pre",
                    crate::HookContributionPhase::Post => "post",
                }),
            );
            data.insert(
                "event".to_owned(),
                serde_json::json!(match contribution.event() {
                    crate::HookContributionEvent::ToolUse => "tool_use",
                    crate::HookContributionEvent::Turn => "turn",
                    crate::HookContributionEvent::Session => "session",
                    crate::HookContributionEvent::UserPrompt => "user_prompt",
                    crate::HookContributionEvent::Subagent => "subagent",
                    crate::HookContributionEvent::McpServer => "mcp_server",
                }),
            );
            data.insert(
                "handler".to_owned(),
                serde_json::json!(match contribution.handler() {
                    crate::HookContributionHandler::Command => "command",
                    crate::HookContributionHandler::Prompt => "prompt",
                    crate::HookContributionHandler::Subagent => "subagent",
                    crate::HookContributionHandler::McpTool => "mcp_tool",
                }),
            );
            data.insert(
                "textBytes".to_owned(),
                serde_json::json!(contribution.text().len()),
            );
            data.insert(
                "untrusted".to_owned(),
                serde_json::json!(contribution.boundary().is_some()),
            );
        }
        SessionEventKind::SessionCreated { .. }
        | SessionEventKind::RuntimeLinked { .. }
        | SessionEventKind::RuntimeConfigured { .. }
        | SessionEventKind::GoalChange { .. }
        | SessionEventKind::WorkflowChange { .. }
        | SessionEventKind::ScheduleChange { .. }
        | SessionEventKind::TeamChange { .. }
        | SessionEventKind::WorkChange { .. }
        | SessionEventKind::ReviewChange { .. }
        | SessionEventKind::CodeModeChange { .. }
        | SessionEventKind::UserMessage { .. }
        | SessionEventKind::SessionActivated {}
        | SessionEventKind::SessionTitle { .. } => {
            data.insert("contentRedacted".to_owned(), serde_json::json!(true));
        }
    }
    serde_json::Value::Object(data)
}

fn insert_turn_step(data: &mut serde_json::Map<String, serde_json::Value>, turn: u64, step: u32) {
    data.insert("turn".to_owned(), serde_json::json!(turn));
    data.insert("step".to_owned(), serde_json::json!(step));
}

fn insert_numbers<const N: usize>(
    data: &mut serde_json::Map<String, serde_json::Value>,
    fields: [(&str, u64); N],
) {
    for (name, value) in fields {
        data.insert(name.to_owned(), serde_json::json!(value));
    }
}

fn redacted_usage(usage: heycode_core::TokenUsage) -> serde_json::Value {
    serde_json::json!({
        "promptTokens":usage.prompt_tokens,
        "completionTokens":usage.completion_tokens
    })
}

fn redacted_cache_usage(usage: heycode_core::ProviderCacheUsage) -> serde_json::Value {
    serde_json::json!({
        "inputTokens":usage.input_tokens(),
        "outputTokens":usage.output_tokens(),
        "cacheReadTokens":usage.cache_read_tokens(),
        "cacheWriteTokens":usage.reported_cache_write_tokens(),
        "uncachedInputTokens":usage.uncached_input_tokens(),
        "cacheWrite5mTokens":usage.cache_write_5m_tokens(),
        "cacheWrite1hTokens":usage.cache_write_1h_tokens(),
        "reasoningTokens":usage.reasoning_tokens()
    })
}

fn turn_end_reason(reason: crate::TurnEndReason) -> &'static str {
    match reason {
        crate::TurnEndReason::Stop => "stop",
        crate::TurnEndReason::MaxTokens => "max_tokens",
        crate::TurnEndReason::MaxSteps => "max_steps",
        crate::TurnEndReason::MaxElapsed => "max_elapsed",
        crate::TurnEndReason::MaxToolCalls => "max_tool_calls",
        crate::TurnEndReason::UnreportedTokenUsage => "unreported_token_usage",
        crate::TurnEndReason::ClockUnavailable => "clock_unavailable",
        crate::TurnEndReason::Error => "error",
        crate::TurnEndReason::Aborted => "aborted",
    }
}

fn lineage_sessions(
    root: &Path,
    id: &heycode_core::SessionId,
) -> Result<Vec<Session>, crate::SessionQueryError> {
    let mut sessions = Vec::new();
    let mut next = id.clone();
    let mut visited = BTreeSet::new();
    loop {
        if sessions.len() == MAX_EXPORT_ANCESTORS || !visited.insert(next.as_str().to_owned()) {
            return Err(crate::SessionQueryError::InvalidSession);
        }
        let session =
            Session::open(root.join(next.as_str())).map_err(crate::query::map_open_error)?;
        let parent = session
            .lineage()
            .map(|lineage| lineage.parent_session_id().clone());
        sessions.push(session);
        let Some(parent) = parent else {
            break;
        };
        next = parent;
    }
    Ok(sessions)
}

fn render_markdown(session: &Session) -> Result<String, crate::SessionQueryError> {
    let title = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::SessionTitle { title } => crate::query::safe_title(title),
            _ => None,
        })
        .unwrap_or_else(|| format!("Session {}", session.id().as_str()));
    let mut output = format!("# {title}\n\n");
    for event in session.events() {
        match &event.kind {
            SessionEventKind::UserMessage { text } => {
                append_markdown_section(&mut output, "User", text)?;
            }
            SessionEventKind::HookContribution { contribution } => {
                append_markdown_section(
                    &mut output,
                    &format!(
                        "Hook {} · {:?}/{:?} · {:?}",
                        contribution.owner(),
                        contribution.phase(),
                        contribution.event(),
                        contribution.handler()
                    ),
                    &contribution.render_for_model(),
                )?;
            }
            SessionEventKind::UserAttachments { attachments, .. } => {
                let names = attachments
                    .iter()
                    .map(|attachment| attachment.display_name().unwrap_or("unnamed attachment"))
                    .collect::<Vec<_>>()
                    .join("\n");
                append_markdown_section(&mut output, "Attachments", &names)?;
            }
            SessionEventKind::AssistantMessage {
                content, reasoning, ..
            } => {
                if let Some(reasoning) = reasoning {
                    append_markdown_section(&mut output, "Assistant reasoning", reasoning)?;
                }
                append_markdown_section(&mut output, "Assistant", content)?;
            }
            SessionEventKind::AssistantAudio { attachments, .. } => {
                let rows = attachments
                    .iter()
                    .filter_map(|attachment| {
                        let audio = attachment.audio()?;
                        Some(format!(
                            "{} · {} · {} ms · {} Hz · {} channel(s) · {}-bit",
                            attachment.display_name().unwrap_or("assistant audio"),
                            attachment.media_type().as_str(),
                            audio.duration_ms(),
                            audio.sample_rate_hz(),
                            audio.channels(),
                            audio.bits_per_sample()
                        ))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                append_markdown_section(&mut output, "Assistant audio", &rows)?;
            }
            SessionEventKind::ToolResult {
                content, is_error, ..
            } => append_markdown_section(
                &mut output,
                if *is_error { "Tool error" } else { "Tool" },
                content,
            )?,
            SessionEventKind::RichToolResult {
                result, is_error, ..
            } => append_markdown_section(
                &mut output,
                if *is_error { "Tool error" } else { "Tool" },
                &result.render_for_model(),
            )?,
            SessionEventKind::CompactionApplied { summary, .. } => {
                append_markdown_section(&mut output, "Portable compaction", summary)?;
            }
            SessionEventKind::NativeCompactionApplied { strategy, .. } => {
                append_markdown_section(
                    &mut output,
                    "Native compaction",
                    &format!("Checkpoint strategy: {strategy}"),
                )?;
            }
            _ => {}
        }
    }
    Ok(output)
}

fn append_markdown_section(
    output: &mut String,
    heading: &str,
    content: &str,
) -> Result<(), crate::SessionQueryError> {
    let addition = format!("## {heading}\n\n{content}\n\n");
    if output.len().saturating_add(addition.len()) > MAX_MARKDOWN_EXPORT_BYTES {
        return Err(crate::SessionQueryError::ExportFailed);
    }
    output.push_str(&addition);
    Ok(())
}

fn ensure_private_directory(root: &Path, name: &str) -> Result<PathBuf, crate::SessionQueryError> {
    let directory = root.join(name);
    let mut created = false;
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(crate::SessionQueryError::StoreUnavailable);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&directory).map_err(|_| crate::SessionQueryError::StoreUnavailable)?;
            created = true;
        }
        Err(_) => return Err(crate::SessionQueryError::StoreUnavailable),
    }
    set_private_directory_mode(&directory)?;
    if created {
        sync_directory(root)?;
    }
    Ok(directory)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), crate::SessionQueryError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_file_mode(&mut options);
    let mut file = options
        .open(path)
        .map_err(|_| crate::SessionQueryError::ExportFailed)?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_data())
        .map_err(|_| crate::SessionQueryError::ExportFailed)
}

#[cfg(unix)]
fn set_private_file_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_private_directory_mode(directory: &Path) -> Result<(), crate::SessionQueryError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| crate::SessionQueryError::StoreUnavailable)
}

#[cfg(not(unix))]
fn set_private_directory_mode(_directory: &Path) -> Result<(), crate::SessionQueryError> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_directory(directory: &Path) -> Result<(), crate::SessionQueryError> {
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| crate::SessionQueryError::StoreUnavailable)
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_directory: &Path) -> Result<(), crate::SessionQueryError> {
    Ok(())
}

fn unsafe_title_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Event kinds startup writes on its own: they say nothing about whether the
/// user did anything with the session.
pub const HOUSEKEEPING_EVENT_KINDS: &[&str] = &[
    "session/created",
    "session/activated",
    "runtime/linked",
    "runtime/configured",
];

/// Whether a log holds only startup housekeeping — see
/// [`HOUSEKEEPING_EVENT_KINDS`] — and therefore represents a session nobody
/// used.
#[must_use]
pub fn is_unused_session(events: &[crate::SessionEvent]) -> bool {
    events
        .iter()
        .all(|event| HOUSEKEEPING_EVENT_KINDS.contains(&event.kind.name()))
}

/// Remove a session that was created and never used.
///
/// Opening the TUI and closing it again without sending anything used to
/// leave a session directory behind every time, so the picker filled with
/// identical empty rows. This removes `root/<id>` only when its log holds
/// nothing but startup housekeeping (`session/created`, `runtime/linked`) —
/// the check is here, at the store, not in the caller — and answers whether it
/// did. A session with any turn, title, or lineage is never touched; use the
/// recoverable delete for those.
///
/// # Errors
/// Filesystem failures reading or removing the directory. A missing
/// directory is `Ok(false)`.
pub fn discard_unused_session(root: &Path, id: &heycode_core::SessionId) -> io::Result<bool> {
    let directory = root.join(id.as_str());
    let log = directory.join("session.jsonl");
    let raw = match fs::read_to_string(&log) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut lines = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .peekable();
    if lines.peek().is_none() {
        return Ok(false);
    }
    let only_housekeeping = lines.all(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| {
                value
                    .get("kind")
                    .and_then(|kind| kind.as_str().map(str::to_owned))
            })
            .is_some_and(|kind| HOUSEKEEPING_EVENT_KINDS.contains(&kind.as_str()))
    });
    if !only_housekeeping {
        return Ok(false);
    }
    // Nothing else may have appeared under the directory (a title marker, an
    // archive marker, a lock still held): then it is not unused.
    let extra = fs::read_dir(&directory)?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name() != "session.jsonl");
    if extra {
        return Ok(false);
    }
    fs::remove_dir_all(&directory)?;
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod discard_tests {
    use super::*;

    #[test]
    fn only_a_creation_only_log_is_discarded() {
        let root = tempfile::tempdir().unwrap();
        let id = heycode_core::SessionId::from_raw("aaaa");
        let directory = root.path().join("aaaa");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("session.jsonl"),
            "{\"v\":2,\"seq\":0,\"time_ms\":1,\"kind\":\"session/created\",\"data\":{}}\n",
        )
        .unwrap();
        assert!(discard_unused_session(root.path(), &id).unwrap());
        assert!(!directory.exists());
        assert!(
            !discard_unused_session(root.path(), &id).unwrap(),
            "gone is a quiet false, not an error"
        );

        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("session.jsonl"),
            "{\"v\":2,\"seq\":0,\"time_ms\":1,\"kind\":\"session/created\",\"data\":{}}\n{\"v\":2,\"seq\":1,\"time_ms\":2,\"kind\":\"runtime/linked\",\"data\":{\"runtime\":\"native\",\"runtime_session_id\":\"aaaa\"}}\n",
        )
        .unwrap();
        assert!(
            discard_unused_session(root.path(), &id).unwrap(),
            "the runtime link startup writes is housekeeping, not use"
        );

        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("session.jsonl"),
            "{\"v\":2,\"seq\":0,\"time_ms\":1,\"kind\":\"session/created\",\"data\":{}}\n{\"v\":2,\"seq\":1,\"time_ms\":2,\"kind\":\"user/message\",\"data\":{\"text\":\"hi\"}}\n",
        )
        .unwrap();
        assert!(!discard_unused_session(root.path(), &id).unwrap());
        assert!(directory.exists(), "a used session is never removed here");

        fs::write(
            directory.join("session.jsonl"),
            "{\"v\":2,\"seq\":0,\"time_ms\":1,\"kind\":\"session/created\",\"data\":{}}\n",
        )
        .unwrap();
        fs::write(directory.join("title"), "x").unwrap();
        assert!(!discard_unused_session(root.path(), &id).unwrap());
        assert!(directory.exists(), "extra durable state means not unused");
    }
}
