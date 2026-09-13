//! Durable structured work, independent from agent conversations and process jobs.

use crate::{SessionEvent, SessionEventKind, TeamId, TeamTask, TeamTaskId, TeamTaskState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ITEMS: usize = 4096;
const MAX_DEPENDENCIES: usize = 64;

/// Stable work identity, never an agent or process identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkItemId(String);

impl WorkItemId {
    /// Validate an opaque work id.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkError> {
        let value = value.into();
        if !valid_id(&value) {
            return Err(WorkError::Invalid);
        }
        Ok(Self(value))
    }
    /// Exact stable id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Ownership boundary of a board entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "team_id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum WorkScope {
    /// Shared work in this session.
    Session,
    /// Shared work belonging to one team.
    Team(TeamId),
}

/// Explicit work outcome; worker exit never implies completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    /// Ready to be considered.
    Pending,
    /// Actively being worked on; multiple entries may be active.
    InProgress,
    /// Waiting on input or a prerequisite.
    Blocked,
    /// Verified work is complete.
    Completed,
    /// Work failed.
    Failed,
    /// Work was cancelled.
    Cancelled,
    /// Durable deletion tombstone.
    Deleted,
}

/// Editable fields of a work item; revisions and identity are server owned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItemFields {
    /// Short human-facing title.
    pub subject: String,
    /// Full instructions or acceptance criteria.
    #[serde(default)]
    pub description: String,
    /// Current explicit outcome.
    pub status: WorkStatus,
    /// Assigned agent/member identity, if any.
    #[serde(default)]
    pub owner: Option<String>,
    /// Prerequisite work entries within the same scope.
    #[serde(default)]
    pub dependencies: Vec<WorkItemId>,
    /// Bounded caller metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl WorkItemFields {
    /// Validate local bounds before reading or changing durable state.
    pub fn validate(&self) -> Result<(), WorkError> {
        if self.subject.trim().is_empty()
            || self.subject.len() > 256
            || self.subject.chars().any(char::is_control)
            || self.description.len() > 16384
            || self
                .description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            || self.owner.as_ref().is_some_and(|owner| !valid_id(owner))
            || self.dependencies.len() > MAX_DEPENDENCIES
            || self.dependencies.iter().any(|id| !valid_id(id.as_str()))
            || self.dependencies.iter().collect::<BTreeSet<_>>().len() != self.dependencies.len()
            || self.metadata.len() > 64
            || self.metadata.keys().any(|key| !valid_id(key))
            || serde_json::to_vec(&self.metadata)
                .map_err(|_| WorkError::Invalid)?
                .len()
                > 8192
        {
            return Err(WorkError::Invalid);
        }
        Ok(())
    }
}

/// One revisioned durable work record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    id: WorkItemId,
    scope: WorkScope,
    revision: u64,
    creation_fingerprint: String,
    fields: WorkItemFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_task_id: Option<TeamTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_summary: Option<String>,
}

impl WorkItem {
    /// Stable identity.
    #[must_use]
    pub const fn id(&self) -> &WorkItemId {
        &self.id
    }
    /// Shared board boundary.
    #[must_use]
    pub const fn scope(&self) -> &WorkScope {
        &self.scope
    }
    /// Latest revision for compare-and-set updates.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Current content and outcome.
    #[must_use]
    pub const fn fields(&self) -> &WorkItemFields {
        &self.fields
    }
    /// Latest team execution result, independent from work instructions.
    #[must_use]
    pub fn result_summary(&self) -> Option<&str> {
        self.result_summary.as_deref()
    }

    /// Existing team task identity when this is a view over team-owned work.
    #[must_use]
    pub const fn team_task_id(&self) -> Option<&TeamTaskId> {
        self.team_task_id.as_ref()
    }

    /// Adapt team-owned durable work without copying it into a second event stream.
    pub fn from_team_task(team: &TeamId, task: &TeamTask) -> Result<Self, WorkError> {
        let fields = WorkItemFields {
            subject: task.title().into(),
            description: task.description().into(),
            status: match task.state() {
                TeamTaskState::Pending => WorkStatus::Pending,
                TeamTaskState::InProgress => WorkStatus::InProgress,
                TeamTaskState::Blocked => WorkStatus::Blocked,
                TeamTaskState::Completed => WorkStatus::Completed,
                TeamTaskState::Failed => WorkStatus::Failed,
                TeamTaskState::Cancelled => WorkStatus::Cancelled,
                TeamTaskState::Deleted => WorkStatus::Deleted,
            },
            owner: task.assigned().then(|| task.assignee().as_str().into()),
            dependencies: task
                .dependencies()
                .iter()
                .map(|dependency| team_work_id(team, dependency))
                .collect::<Result<Vec<_>, _>>()?,
            metadata: task.metadata().clone(),
        };
        // Team result summaries have a separate, larger bound. Keep their full source in the team log.
        let mut fields = fields;
        fields.description = bounded_text(&clean_legacy_text(&fields.description), 16384).into();
        let status = fields.status;
        if status == WorkStatus::Deleted {
            fields.status = WorkStatus::Pending;
        }
        let mut item =
            WorkChange::create(WorkScope::Team(team.clone()), task.id().as_str(), fields)?.item;
        item.fields.status = status;
        item.revision = task.revision();
        item.team_task_id = Some(task.id().clone());
        item.result_summary = task.result_summary().map(str::to_owned);
        if let Some(fingerprint) = task.creation_fingerprint() {
            item.creation_fingerprint = fingerprint.into();
        }
        Ok(item)
    }

    /// Original create fingerprint used to reconcile repeated creates after updates.
    #[must_use]
    pub fn creation_fingerprint(&self) -> &str {
        &self.creation_fingerprint
    }
}

/// A strict version-one full-record mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkChange {
    version: u8,
    expected_revision: Option<u64>,
    item: WorkItem,
}

impl WorkChange {
    /// Prepare an idempotent create. The key is scoped to a board and may be retried unchanged.
    pub fn create(
        scope: WorkScope,
        request_key: &str,
        fields: WorkItemFields,
    ) -> Result<Self, WorkError> {
        if !valid_id(request_key) || fields.status == WorkStatus::Deleted {
            return Err(WorkError::Invalid);
        }
        fields.validate()?;
        validate_scope(&scope)?;
        let identity =
            serde_json::to_vec(&(&scope, request_key)).map_err(|_| WorkError::Invalid)?;
        let fingerprint = serde_json::to_vec(&(&scope, &fields)).map_err(|_| WorkError::Invalid)?;
        Ok(Self {
            version: 1,
            expected_revision: None,
            item: WorkItem {
                id: WorkItemId(format!("work-{:x}", Sha256::digest(identity))),
                scope,
                revision: 1,
                creation_fingerprint: format!("{:x}", Sha256::digest(fingerprint)),
                fields,
                team_task_id: None,
                result_summary: None,
            },
        })
    }
    /// Prepare a replacement against the exact revision read by the caller.
    pub fn update(
        current: &WorkItem,
        expected_revision: u64,
        fields: WorkItemFields,
    ) -> Result<Self, WorkError> {
        if current.revision != expected_revision {
            return Err(WorkError::Conflict);
        }
        if current.fields.status == WorkStatus::Deleted {
            return Err(WorkError::Deleted);
        }
        fields.validate()?;
        let mut item = current.clone();
        item.revision = expected_revision.checked_add(1).ok_or(WorkError::Limit)?;
        item.fields = fields;
        Ok(Self {
            version: 1,
            expected_revision: Some(expected_revision),
            item,
        })
    }
    /// Candidate record.
    #[must_use]
    pub const fn item(&self) -> &WorkItem {
        &self.item
    }
    /// Validate deserialized data before replay or append.
    pub fn validate_shape(&self) -> Result<(), WorkError> {
        if self.item.team_task_id.is_some()
            || self.item.result_summary.is_some()
            || self.version != 1
            || !valid_id(self.item.id.as_str())
            || self.item.creation_fingerprint.len() != 64
            || !self
                .item
                .creation_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || self.item.revision
                != self
                    .expected_revision
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or(WorkError::Invalid)?
            || self.expected_revision == Some(0)
            || (self.expected_revision.is_none() && self.item.fields.status == WorkStatus::Deleted)
        {
            return Err(WorkError::Invalid);
        }
        validate_scope(&self.item.scope)?;
        self.item.fields.validate()
    }
}

/// Recoverable work-domain failure with a stable actionable class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkError {
    /// Malformed or oversized fields.
    #[error("invalid work fields or bounds")]
    Invalid,
    /// Compare-and-set or repeated-create payload conflict.
    #[error("work revision or idempotency conflict; read the current item before retrying")]
    Conflict,
    /// Unknown item or prerequisite.
    #[error("work item or dependency does not exist")]
    Unknown,
    /// Deleted records cannot be revived implicitly.
    #[error("work item is deleted")]
    Deleted,
    /// Dependencies would cross board boundaries.
    #[error("work dependency must belong to the same scope")]
    Scope,
    /// Cyclic work is not executable.
    #[error("work dependencies contain a cycle")]
    Cycle,
    /// Cannot activate/complete dependent work or delete a live prerequisite.
    #[error("work has unfinished dependencies or live dependents")]
    Blocked,
    /// Bounded board/revision space exhausted.
    #[error("work board limit reached")]
    Limit,
}

/// Reconstructed board, containing tombstones for durable identity and replay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkProjection {
    items: BTreeMap<WorkItemId, WorkItem>,
}

impl WorkProjection {
    /// Exact record, including tombstones.
    #[must_use]
    pub fn get(&self, id: &WorkItemId) -> Option<&WorkItem> {
        self.items.get(id)
    }
    /// Stable identity order; callers choose whether to display tombstones.
    pub fn items(&self) -> impl Iterator<Item = &WorkItem> {
        self.items.values()
    }
    /// Apply the common work invariants to a team-owned replacement without writing a second log event.
    pub fn validate_replacement(
        &self,
        id: &WorkItemId,
        expected_revision: u64,
        fields: WorkItemFields,
    ) -> Result<(), WorkError> {
        let mut current = self.get(id).ok_or(WorkError::Unknown)?.clone();
        current.team_task_id = None;
        current.result_summary = None;
        let change = WorkChange::update(&current, expected_revision, fields)?;
        self.clone().apply(&change).map(|_| ())
    }

    /// Validate and apply one change atomically. Returns false for an identical create retry.
    pub fn apply(&mut self, change: &WorkChange) -> Result<bool, WorkError> {
        change.validate_shape()?;
        let item = &change.item;
        match (self.items.get(&item.id), change.expected_revision) {
            (Some(existing), None)
                if existing.creation_fingerprint == item.creation_fingerprint =>
            {
                return Ok(false);
            }
            (Some(_), None) | (None, Some(_)) => return Err(WorkError::Conflict),
            (Some(existing), Some(revision)) => {
                if existing.revision != revision
                    || existing.scope != item.scope
                    || existing.creation_fingerprint != item.creation_fingerprint
                {
                    return Err(WorkError::Conflict);
                }
                if existing.fields.status == WorkStatus::Deleted {
                    return Err(WorkError::Deleted);
                }
            }
            (None, None) if self.items.len() >= MAX_ITEMS => return Err(WorkError::Limit),
            (None, None) => {}
        }
        for dependency in &item.fields.dependencies {
            if dependency == &item.id {
                return Err(WorkError::Cycle);
            }
            let target = self.items.get(dependency).ok_or(WorkError::Unknown)?;
            if target.scope != item.scope {
                return Err(WorkError::Scope);
            }
            if target.fields.status == WorkStatus::Deleted {
                return Err(WorkError::Deleted);
            }
            if matches!(
                item.fields.status,
                WorkStatus::InProgress | WorkStatus::Completed
            ) && target.fields.status != WorkStatus::Completed
            {
                return Err(WorkError::Blocked);
            }
        }
        // Check the prospective graph iteratively so hostile chains cannot overflow the stack.
        let mut visited = BTreeSet::new();
        let mut pending = item.fields.dependencies.clone();
        while let Some(id) = pending.pop() {
            if id == item.id {
                return Err(WorkError::Cycle);
            }
            if visited.insert(id.clone()) {
                pending.extend(
                    self.items
                        .get(&id)
                        .ok_or(WorkError::Unknown)?
                        .fields
                        .dependencies
                        .iter()
                        .cloned(),
                );
            }
        }
        if item.fields.status != WorkStatus::Completed
            && self.items.values().any(|other| {
                matches!(
                    other.fields.status,
                    WorkStatus::InProgress | WorkStatus::Completed
                ) && other.fields.dependencies.contains(&item.id)
            })
        {
            return Err(WorkError::Blocked);
        }
        if item.fields.status == WorkStatus::Deleted
            && self.items.values().any(|other| {
                other.fields.status != WorkStatus::Deleted
                    && other.fields.dependencies.contains(&item.id)
            })
        {
            return Err(WorkError::Blocked);
        }
        self.items.insert(item.id.clone(), item.clone());
        Ok(true)
    }
}

/// Replay structured changes and migrate successful historical TodoWrite results without rewriting logs.
pub fn project_work_items(events: &[SessionEvent]) -> Result<WorkProjection, WorkError> {
    let mut projection = WorkProjection::default();
    let mut old_calls = BTreeSet::new();
    let mut native_seen = false;
    for event in events {
        match &event.kind {
            SessionEventKind::WorkChange { change } => {
                projection.apply(change)?;
                native_seen = true;
            }
            SessionEventKind::ToolCall { call_id, name, .. }
                if !native_seen
                    && matches!(name.as_str(), "todo_write" | "mcp__heycode__todo_write") =>
            {
                old_calls.insert(call_id.as_str().to_owned());
            }
            SessionEventKind::ToolResult {
                call_id,
                content,
                is_error: false,
                ..
            } if !native_seen && old_calls.remove(call_id.as_str()) => {
                let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(content) else {
                    continue;
                };
                if rows.len() > MAX_ITEMS {
                    return Err(WorkError::Limit);
                }
                let mut migrated = WorkProjection::default();
                for row in rows {
                    let Some(subject) = row.get("content").and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let status = match row.get("status").and_then(serde_json::Value::as_str) {
                        Some("pending") => WorkStatus::Pending,
                        Some("in_progress") => WorkStatus::InProgress,
                        Some("completed") => WorkStatus::Completed,
                        _ => continue,
                    };
                    let key = format!("legacy-todo-{:x}", Sha256::digest(subject.as_bytes()));
                    let change = WorkChange::create(
                        WorkScope::Session,
                        &key,
                        WorkItemFields {
                            subject: {
                                let cleaned =
                                    clean_legacy_text(subject).replace(['\n', '\r', '\t'], " ");
                                let trimmed = cleaned.trim();
                                if trimmed.is_empty() {
                                    "Legacy work item".into()
                                } else {
                                    bounded_text(trimmed, 256).into()
                                }
                            },
                            description: if subject.len() > 256 {
                                bounded_text(&clean_legacy_text(subject), 16384).to_owned()
                            } else {
                                String::new()
                            },
                            status,
                            owner: None,
                            dependencies: Vec::new(),
                            metadata: BTreeMap::from([
                                ("legacy_source_seq".into(), serde_json::json!(event.seq)),
                                (
                                    "legacy_content_truncated".into(),
                                    serde_json::json!(subject.len() > 16384),
                                ),
                            ]),
                        },
                    )?;
                    migrated.apply(&change)?;
                }
                projection = migrated;
            }
            _ => {}
        }
    }
    // Team mutations retain their existing authority and transaction owner. This is a view,
    // not a second write or a separately mutable copy of team state.
    for team in crate::project_teams(events)
        .map_err(|_| WorkError::Conflict)?
        .teams()
    {
        for task in team.tasks() {
            let item = WorkItem::from_team_task(team.id(), task)?;
            if projection.items.insert(item.id.clone(), item).is_some() {
                return Err(WorkError::Conflict);
            }
        }
    }
    Ok(projection)
}

/// Stable identity for a team-owned work entry, independent of its display title.
pub fn team_work_id(team: &TeamId, task: &TeamTaskId) -> Result<WorkItemId, WorkError> {
    let identity = serde_json::to_vec(&(WorkScope::Team(team.clone()), task.as_str()))
        .map_err(|_| WorkError::Invalid)?;
    WorkItemId::new(format!("work-{:x}", Sha256::digest(identity)))
}

fn clean_legacy_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}

fn bounded_text(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}
fn validate_scope(scope: &WorkScope) -> Result<(), WorkError> {
    if let WorkScope::Team(id) = scope {
        TeamId::new(id.as_str()).map_err(|_| WorkError::Invalid)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    fn fields(subject: &str) -> WorkItemFields {
        WorkItemFields {
            subject: subject.into(),
            description: String::new(),
            status: WorkStatus::Pending,
            owner: None,
            dependencies: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
    fn event(seq: u64, change: WorkChange) -> SessionEvent {
        SessionEvent {
            v: crate::CURRENT_SESSION_LOG_VERSION,
            seq,
            time_ms: 0,
            kind: SessionEventKind::WorkChange {
                change: Box::new(change),
            },
        }
    }
    #[test]
    fn revisions_are_per_item_and_create_retries_survive_later_updates() {
        let a = WorkChange::create(WorkScope::Session, "a", fields("First")).unwrap();
        let b = WorkChange::create(WorkScope::Session, "b", fields("Second")).unwrap();
        let mut board = WorkProjection::default();
        board.apply(&a).unwrap();
        board.apply(&b).unwrap();
        let mut active_a = fields("First");
        active_a.status = WorkStatus::InProgress;
        let mut active_b = fields("Second");
        active_b.status = WorkStatus::InProgress;
        let ua = WorkChange::update(a.item(), 1, active_a).unwrap();
        let ub = WorkChange::update(b.item(), 1, active_b).unwrap();
        board.apply(&ua).unwrap();
        board.apply(&ub).unwrap();
        assert_eq!(board.apply(&a), Ok(false));
        assert_eq!(board.apply(&ua), Err(WorkError::Conflict));
        assert_eq!(
            board
                .items()
                .filter(|item| item.fields.status == WorkStatus::InProgress)
                .count(),
            2
        );
        let conflict =
            WorkChange::create(WorkScope::Session, "a", fields("Changed request")).unwrap();
        assert_eq!(board.apply(&conflict), Err(WorkError::Conflict));
        let events = [event(0, a), event(1, b), event(2, ua), event(3, ub)];
        let decoded = events
            .iter()
            .map(|event| SessionEvent {
                kind: serde_json::from_str(&serde_json::to_string(&event.kind).unwrap()).unwrap(),
                ..event.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(project_work_items(&decoded).unwrap(), board);
    }
    #[test]
    fn dependencies_enforce_scope_cycles_readiness_and_tombstones_without_partial_mutation() {
        let a = WorkChange::create(WorkScope::Session, "a", fields("First")).unwrap();
        let mut b_fields = fields("Second");
        b_fields.dependencies.push(a.item.id.clone());
        let b = WorkChange::create(WorkScope::Session, "b", b_fields.clone()).unwrap();
        let mut board = WorkProjection::default();
        board.apply(&a).unwrap();
        board.apply(&b).unwrap();
        let original = board.clone();
        let mut cycle = fields("First");
        cycle.dependencies.push(b.item.id.clone());
        assert_eq!(
            board.apply(&WorkChange::update(a.item(), 1, cycle).unwrap()),
            Err(WorkError::Cycle)
        );
        b_fields.status = WorkStatus::InProgress;
        assert_eq!(
            board.apply(&WorkChange::update(b.item(), 1, b_fields.clone()).unwrap()),
            Err(WorkError::Blocked)
        );
        let mut deleted = fields("First");
        deleted.status = WorkStatus::Deleted;
        assert_eq!(
            board.apply(&WorkChange::update(a.item(), 1, deleted).unwrap()),
            Err(WorkError::Blocked)
        );
        let mut cross = fields("Team work");
        cross.dependencies.push(a.item.id.clone());
        let cross = WorkChange::create(
            WorkScope::Team(TeamId::new("team").unwrap()),
            "cross",
            cross,
        )
        .unwrap();
        assert_eq!(board.apply(&cross), Err(WorkError::Scope));
        assert_eq!(board, original);
        let mut done = fields("First");
        done.status = WorkStatus::Completed;
        board
            .apply(&WorkChange::update(a.item(), 1, done).unwrap())
            .unwrap();
        board
            .apply(&WorkChange::update(b.item(), 1, b_fields).unwrap())
            .unwrap();
        let mut remove = board.get(b.item.id()).unwrap().fields.clone();
        remove.status = WorkStatus::Deleted;
        let deletion = WorkChange::update(board.get(b.item.id()).unwrap(), 2, remove).unwrap();
        board.apply(&deletion).unwrap();
        assert_eq!(
            WorkChange::update(deletion.item(), 3, fields("Revive")),
            Err(WorkError::Deleted)
        );
    }
    #[test]
    fn legacy_todos_migrate_and_native_changes_survive_replay() {
        let call_id = heycode_core::CallId::from_raw("old-todo");
        let mut events = vec![
            SessionEvent {
                v: crate::CURRENT_SESSION_LOG_VERSION,
                seq: 0,
                time_ms: 0,
                kind: SessionEventKind::ToolCall {
                    turn: 0,
                    call_id: call_id.clone(),
                    name: "todo_write".into(),
                    args: serde_json::json!({}),
                },
            },
            SessionEvent {
                v: crate::CURRENT_SESSION_LOG_VERSION,
                seq: 1,
                time_ms: 0,
                kind: SessionEventKind::ToolResult {
                    call_id,
                    content: r#"[{"content":"Old work","status":"in_progress"}]"#.into(),
                    is_error: false,
                    untrusted_content: None,
                },
            },
        ];
        let old = project_work_items(&events)
            .unwrap()
            .items()
            .next()
            .unwrap()
            .clone();
        let mut done = old.fields.clone();
        done.status = WorkStatus::Completed;
        events.push(event(2, WorkChange::update(&old, 1, done).unwrap()));
        let board = project_work_items(&events).unwrap();
        assert_eq!(board.items().count(), 1);
        assert_eq!(board.get(old.id()).unwrap().revision(), 2);
        assert_eq!(
            board.get(old.id()).unwrap().fields.status,
            WorkStatus::Completed
        );
    }
    #[test]
    fn legacy_todo_controls_and_oversized_unicode_remain_replayable() {
        let original = format!("\u{1b}[31m\0{}\nTail", "界".repeat(7000));
        let call_id = heycode_core::CallId::from_raw("legacy-large");
        let events = vec![
            SessionEvent {
                v: crate::CURRENT_SESSION_LOG_VERSION,
                seq: 0,
                time_ms: 0,
                kind: SessionEventKind::ToolCall {
                    turn: 0,
                    call_id: call_id.clone(),
                    name: "mcp__heycode__todo_write".into(),
                    args: serde_json::json!({}),
                },
            },
            SessionEvent {
                v: crate::CURRENT_SESSION_LOG_VERSION,
                seq: 1,
                time_ms: 0,
                kind: SessionEventKind::ToolResult {
                    call_id,
                    content: serde_json::json!([{ "content":original, "status":"pending" }])
                        .to_string(),
                    is_error: false,
                    untrusted_content: None,
                },
            },
        ];
        let board = project_work_items(&events).unwrap();
        let item = board.items().next().unwrap();
        assert!(item.fields.subject.len() <= 256);
        assert!(item.fields.description.len() <= 16384);
        assert!(!item.fields.subject.chars().any(char::is_control));
        assert!(!item.fields.description.contains(['\u{1b}', '\0']));
        assert_eq!(item.fields.metadata["legacy_content_truncated"], true);
        assert_eq!(project_work_items(&events).unwrap(), board);
        let SessionEventKind::ToolResult { content, .. } = &events[1].kind else {
            panic!("result missing")
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(content).unwrap()[0]["content"],
            original
        );
    }
}
