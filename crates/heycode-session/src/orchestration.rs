//! Durable orchestration vocabulary and strict replay projections.

use crate::{WorkflowAgentRecord, WorkflowJobRecord, WorkflowPhase};
use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Local, LocalResult, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{InboxProjection, InboxSource, SessionEvent, SessionEventKind};

const GOAL_CHANGE_VERSION: u8 = 1;
const MAX_GOAL_ID_BYTES: usize = 128;
const MAX_GOAL_OBJECTIVE_BYTES: usize = 64 * 1024;
const MAX_GOAL_ROUNDS: u32 = 1_000_000;
const MAX_BLOCK_CODE_BYTES: usize = 64;
const MAX_BLOCK_MESSAGE_BYTES: usize = 8 * 1024;

fn valid_opaque_id(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max_bytes
        && !value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
}

/// Stable same-session goal identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GoalId(String);

impl GoalId {
    /// Validate externally supplied goal identity.
    ///
    /// # Errors
    /// Blank, oversized, or structurally unsafe values are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, GoalProjectionError> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    /// Mint one fresh goal identity.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Borrow the opaque identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), GoalProjectionError> {
        if valid_opaque_id(&self.0, MAX_GOAL_ID_BYTES) {
            Ok(())
        } else {
            Err(GoalProjectionError::Invalid("goal id is invalid"))
        }
    }
}

impl std::fmt::Display for GoalId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Compare-and-set reference to one exact goal revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalRef {
    id: GoalId,
    revision: u64,
}

impl GoalRef {
    /// Construct an exact positive revision reference.
    ///
    /// # Errors
    /// Invalid identity or a zero revision is refused.
    pub fn new(id: GoalId, revision: u64) -> Result<Self, GoalProjectionError> {
        let reference = Self { id, revision };
        reference.validate()?;
        Ok(reference)
    }

    /// Goal identity.
    #[must_use]
    pub const fn id(&self) -> &GoalId {
        &self.id
    }

    /// Positive revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    fn validate(&self) -> Result<(), GoalProjectionError> {
        self.id.validate()?;
        if self.revision == 0 {
            Err(GoalProjectionError::Invalid(
                "goal revision must be positive",
            ))
        } else {
            Ok(())
        }
    }
}

/// Durable goal lifecycle phase. Process-local activation is separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPhase {
    /// Work may continue after an explicit live rearm.
    Active,
    /// A human or policy paused the objective.
    Paused,
    /// Work cannot continue without resolving a durable blocker.
    Blocked,
    /// The objective was declared complete.
    Complete,
}

/// Stable blocked classification plus human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalBlockReason {
    code: String,
    message: String,
}

impl GoalBlockReason {
    /// Validate one blocked reason.
    ///
    /// # Errors
    /// The code must be lower-kebab-case and the message bounded non-empty text.
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, GoalProjectionError> {
        let reason = Self {
            code: code.into(),
            message: message.into(),
        };
        reason.validate()?;
        Ok(reason)
    }

    /// Stable policy code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Human-readable explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn validate(&self) -> Result<(), GoalProjectionError> {
        let valid_code = !self.code.is_empty()
            && self.code.len() <= MAX_BLOCK_CODE_BYTES
            && self.code.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            });
        if !valid_code || !valid_text(&self.message, MAX_BLOCK_MESSAGE_BYTES) {
            Err(GoalProjectionError::Invalid(
                "goal blocked reason is invalid",
            ))
        } else {
            Ok(())
        }
    }
}

/// Complete durable state written by every non-clear goal mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalSnapshot {
    id: GoalId,
    revision: u64,
    objective: String,
    phase: GoalPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<GoalBlockReason>,
    max_rounds: u32,
}

impl GoalSnapshot {
    /// Construct and validate one full snapshot.
    ///
    /// # Errors
    /// Invalid identity/revision/objective/round cap or incoherent blocked data.
    pub fn new(
        id: GoalId,
        revision: u64,
        objective: impl Into<String>,
        phase: GoalPhase,
        blocked_reason: Option<GoalBlockReason>,
        max_rounds: u32,
    ) -> Result<Self, GoalProjectionError> {
        let snapshot = Self {
            id,
            revision,
            objective: objective.into(),
            phase,
            blocked_reason,
            max_rounds,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Goal identity.
    #[must_use]
    pub const fn id(&self) -> &GoalId {
        &self.id
    }

    /// Positive CAS revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Objective text.
    #[must_use]
    pub fn objective(&self) -> &str {
        &self.objective
    }

    /// Durable lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> GoalPhase {
        self.phase
    }

    /// Blocker present exactly for [`GoalPhase::Blocked`].
    #[must_use]
    pub const fn blocked_reason(&self) -> Option<&GoalBlockReason> {
        self.blocked_reason.as_ref()
    }

    /// Total admitted-round cap; zero means unlimited.
    #[must_use]
    pub const fn max_rounds(&self) -> u32 {
        self.max_rounds
    }

    /// Exact reference represented by this snapshot.
    #[must_use]
    pub fn reference(&self) -> GoalRef {
        GoalRef {
            id: self.id.clone(),
            revision: self.revision,
        }
    }

    fn validate(&self) -> Result<(), GoalProjectionError> {
        self.reference().validate()?;
        if !valid_text(&self.objective, MAX_GOAL_OBJECTIVE_BYTES)
            || self.max_rounds > MAX_GOAL_ROUNDS
            || (self.phase == GoalPhase::Blocked) != self.blocked_reason.is_some()
        {
            return Err(GoalProjectionError::Invalid("goal snapshot is invalid"));
        }
        if let Some(reason) = &self.blocked_reason {
            reason.validate()?;
        }
        Ok(())
    }
}

/// State-changing verb recorded in a full-snapshot goal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalOperation {
    /// Establish a new identity at revision one.
    Create,
    /// Change objective and/or round cap without changing phase.
    Edit,
    /// Transition active to paused.
    Pause,
    /// Rearm active work or transition paused/blocked to active.
    Resume,
    /// Mark an unfinished goal complete.
    Complete,
    /// Transition active to blocked with a reason.
    Block,
}

/// Version-one durable goal mutation union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalChange {
    /// Complete post-mutation goal snapshot.
    Snapshot {
        /// Durable payload version.
        version: u8,
        /// Mutation verb.
        action: GoalOperation,
        /// Full post-mutation state.
        goal: GoalSnapshot,
        /// Highest admitted goal round at the mutation.
        rounds_started: u32,
        /// Create mutation time.
        created_at_ms: i64,
        /// Latest mutation time.
        updated_at_ms: i64,
    },
    /// Clear tombstone retaining the final advanced revision.
    Clear {
        /// Durable payload version.
        version: u8,
        /// One revision past the cleared snapshot.
        cleared: GoalRef,
        /// Clear mutation time.
        cleared_at_ms: i64,
    },
}

impl Serialize for GoalChange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SnapshotWire<'a> {
            version: u8,
            operation: GoalOperation,
            goal: &'a GoalSnapshot,
            rounds_started: u32,
            created_at_ms: i64,
            updated_at_ms: i64,
        }
        #[derive(Serialize)]
        struct ClearWire<'a> {
            version: u8,
            operation: &'static str,
            cleared: &'a GoalRef,
            cleared_at_ms: i64,
        }
        match self {
            Self::Snapshot {
                version,
                action,
                goal,
                rounds_started,
                created_at_ms,
                updated_at_ms,
            } => SnapshotWire {
                version: *version,
                operation: *action,
                goal,
                rounds_started: *rounds_started,
                created_at_ms: *created_at_ms,
                updated_at_ms: *updated_at_ms,
            }
            .serialize(serializer),
            Self::Clear {
                version,
                cleared,
                cleared_at_ms,
            } => ClearWire {
                version: *version,
                operation: "clear",
                cleared,
                cleared_at_ms: *cleared_at_ms,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for GoalChange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SnapshotWire {
            version: u8,
            operation: GoalOperation,
            goal: GoalSnapshot,
            rounds_started: u32,
            created_at_ms: i64,
            updated_at_ms: i64,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ClearWire {
            version: u8,
            operation: String,
            cleared: GoalRef,
            cleared_at_ms: i64,
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        let operation = value
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("goal operation is missing"))?;
        if operation == "clear" {
            let wire: ClearWire = serde_json::from_value(value)
                .map_err(|error| serde::de::Error::custom(error.to_string()))?;
            if wire.operation != "clear" {
                return Err(serde::de::Error::custom("goal clear operation is invalid"));
            }
            Ok(Self::Clear {
                version: wire.version,
                cleared: wire.cleared,
                cleared_at_ms: wire.cleared_at_ms,
            })
        } else {
            let wire: SnapshotWire = serde_json::from_value(value)
                .map_err(|error| serde::de::Error::custom(error.to_string()))?;
            Ok(Self::Snapshot {
                version: wire.version,
                action: wire.operation,
                goal: wire.goal,
                rounds_started: wire.rounds_started,
                created_at_ms: wire.created_at_ms,
                updated_at_ms: wire.updated_at_ms,
            })
        }
    }
}

impl GoalChange {
    /// Construct one full-snapshot mutation.
    #[must_use]
    pub const fn snapshot(
        action: GoalOperation,
        goal: GoalSnapshot,
        rounds_started: u32,
        created_at_ms: i64,
        updated_at_ms: i64,
    ) -> Self {
        Self::Snapshot {
            version: GOAL_CHANGE_VERSION,
            action,
            goal,
            rounds_started,
            created_at_ms,
            updated_at_ms,
        }
    }

    /// Construct a clear tombstone.
    #[must_use]
    pub const fn clear(cleared: GoalRef, cleared_at_ms: i64) -> Self {
        Self::Clear {
            version: GOAL_CHANGE_VERSION,
            cleared,
            cleared_at_ms,
        }
    }

    pub(crate) fn validate_shape(&self) -> Result<(), GoalProjectionError> {
        match self {
            Self::Snapshot {
                version,
                goal,
                rounds_started,
                created_at_ms,
                updated_at_ms,
                ..
            } => {
                if *version != GOAL_CHANGE_VERSION
                    || *created_at_ms < 0
                    || updated_at_ms < created_at_ms
                    || (goal.max_rounds != 0 && *rounds_started > goal.max_rounds)
                {
                    return Err(GoalProjectionError::Invalid(
                        "goal snapshot change is invalid",
                    ));
                }
                goal.validate()
            }
            Self::Clear {
                version,
                cleared,
                cleared_at_ms,
            } => {
                if *version != GOAL_CHANGE_VERSION || *cleared_at_ms < 0 {
                    return Err(GoalProjectionError::Invalid("goal clear change is invalid"));
                }
                cleared.validate()
            }
        }
    }
}

/// Current durable goal plus derived admitted-round/timestamp facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalView {
    snapshot: GoalSnapshot,
    rounds_started: u32,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl GoalView {
    /// Full latest snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &GoalSnapshot {
        &self.snapshot
    }

    /// Highest sequential admitted goal round.
    #[must_use]
    pub const fn rounds_started(&self) -> u32 {
        self.rounds_started
    }

    /// Create mutation time.
    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    /// Latest mutation time.
    #[must_use]
    pub const fn updated_at_ms(&self) -> i64 {
        self.updated_at_ms
    }
}

/// Strict replay of one session's current goal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalProjection {
    current: Option<GoalView>,
    last_ref: Option<GoalRef>,
}

impl GoalProjection {
    /// Current goal, absent before create or after clear.
    #[must_use]
    pub const fn current(&self) -> Option<&GoalView> {
        self.current.as_ref()
    }

    /// Latest mutation reference, including a clear tombstone.
    #[must_use]
    pub const fn last_ref(&self) -> Option<&GoalRef> {
        self.last_ref.as_ref()
    }
}

/// Strict goal replay failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GoalProjectionError {
    /// One bounded structural or transition invariant failed.
    #[error("invalid durable goal state: {0}")]
    Invalid(&'static str),
    /// Durable inbox replay failed before goal attribution could be trusted.
    #[error("invalid durable goal inbox attribution")]
    Inbox,
}

/// Rebuild and validate one session's goal domain from durable events.
///
/// # Errors
/// Malformed snapshots, stale revisions, illegal transitions, timestamp/counter
/// regressions, or non-sequential admitted goal rounds fail loud.
pub fn project_goal(events: &[SessionEvent]) -> Result<GoalProjection, GoalProjectionError> {
    let mut projection = GoalProjection::default();
    let mut inbox = InboxProjection::default();
    let mut seen_ids = BTreeSet::new();
    let mut pending_admission: Option<(String, GoalId, u64, u32)> = None;

    for event in events {
        if let Some((text, goal_id, revision, round)) = pending_admission.take() {
            let SessionEventKind::UserMessage { text: admitted } = &event.kind else {
                return Err(GoalProjectionError::Invalid(
                    "goal inbox claim is not followed by its user message",
                ));
            };
            if admitted != &text {
                return Err(GoalProjectionError::Invalid(
                    "goal inbox admission text changed",
                ));
            }
            admit_round(&mut projection, &goal_id, revision, round)?;
        }

        if let SessionEventKind::GoalChange { change } = &event.kind {
            apply_goal_change(&mut projection, &mut seen_ids, change)?;
        }

        let claimed_before = inbox.claimed().len();
        inbox
            .apply_event(event)
            .map_err(|_| GoalProjectionError::Inbox)?;
        for settlement in &inbox.claimed()[claimed_before..] {
            if let InboxSource::Goal {
                goal_id,
                revision,
                round,
            } = settlement.message().source()
            {
                if pending_admission.is_some() {
                    return Err(GoalProjectionError::Invalid(
                        "multiple goal rounds were claimed in one splice",
                    ));
                }
                pending_admission = Some((
                    settlement.message().text().to_owned(),
                    goal_id.clone(),
                    *revision,
                    *round,
                ));
            }
        }
    }
    if pending_admission.is_some() {
        return Err(GoalProjectionError::Invalid(
            "goal inbox claim has no admitted user message",
        ));
    }
    Ok(projection)
}

fn admit_round(
    projection: &mut GoalProjection,
    goal_id: &GoalId,
    revision: u64,
    round: u32,
) -> Result<(), GoalProjectionError> {
    let Some(current) = projection.current.as_mut() else {
        return Err(GoalProjectionError::Invalid(
            "goal round has no current goal",
        ));
    };
    if current.snapshot.id != *goal_id
        || current.snapshot.revision != revision
        || round != current.rounds_started.saturating_add(1)
        || (current.snapshot.max_rounds != 0 && round > current.snapshot.max_rounds)
    {
        return Err(GoalProjectionError::Invalid(
            "goal round attribution is stale or non-sequential",
        ));
    }
    current.rounds_started = round;
    Ok(())
}

fn apply_goal_change(
    projection: &mut GoalProjection,
    seen_ids: &mut BTreeSet<GoalId>,
    change: &GoalChange,
) -> Result<(), GoalProjectionError> {
    change.validate_shape()?;
    match change {
        GoalChange::Snapshot {
            action,
            goal,
            rounds_started,
            created_at_ms,
            updated_at_ms,
            ..
        } => {
            if *action == GoalOperation::Create {
                if goal.revision != 1
                    || *rounds_started != 0
                    || created_at_ms != updated_at_ms
                    || goal.phase != GoalPhase::Active
                    || !seen_ids.insert(goal.id.clone())
                    || projection
                        .current
                        .as_ref()
                        .is_some_and(|current| current.snapshot.phase != GoalPhase::Complete)
                {
                    return Err(GoalProjectionError::Invalid(
                        "goal create transition is invalid",
                    ));
                }
            } else {
                let Some(current) = projection.current.as_ref() else {
                    return Err(GoalProjectionError::Invalid(
                        "goal mutation has no current goal",
                    ));
                };
                validate_transition(
                    current,
                    *action,
                    goal,
                    *rounds_started,
                    *created_at_ms,
                    *updated_at_ms,
                )?;
            }
            projection.last_ref = Some(goal.reference());
            projection.current = Some(GoalView {
                snapshot: goal.clone(),
                rounds_started: *rounds_started,
                created_at_ms: *created_at_ms,
                updated_at_ms: *updated_at_ms,
            });
        }
        GoalChange::Clear {
            cleared,
            cleared_at_ms,
            ..
        } => {
            let Some(current) = projection.current.as_ref() else {
                return Err(GoalProjectionError::Invalid(
                    "goal clear has no current goal",
                ));
            };
            if cleared.id != current.snapshot.id
                || cleared.revision != current.snapshot.revision.saturating_add(1)
                || *cleared_at_ms < current.updated_at_ms
            {
                return Err(GoalProjectionError::Invalid(
                    "goal clear transition is stale",
                ));
            }
            projection.last_ref = Some(cleared.clone());
            projection.current = None;
        }
    }
    Ok(())
}

fn validate_transition(
    current: &GoalView,
    action: GoalOperation,
    next: &GoalSnapshot,
    rounds_started: u32,
    created_at_ms: i64,
    updated_at_ms: i64,
) -> Result<(), GoalProjectionError> {
    let previous = &current.snapshot;
    if next.id != previous.id
        || next.revision != previous.revision.saturating_add(1)
        || rounds_started != current.rounds_started
        || created_at_ms != current.created_at_ms
        || updated_at_ms < current.updated_at_ms
    {
        return Err(GoalProjectionError::Invalid(
            "goal mutation violates its CAS or counters",
        ));
    }
    match action {
        GoalOperation::Create => {
            return Err(GoalProjectionError::Invalid(
                "goal create cannot update an existing snapshot",
            ));
        }
        GoalOperation::Edit => {
            if next.phase != previous.phase || next.blocked_reason != previous.blocked_reason {
                return Err(GoalProjectionError::Invalid(
                    "goal edit changed lifecycle state",
                ));
            }
        }
        GoalOperation::Pause => {
            require_definition(previous, next)?;
            if previous.phase != GoalPhase::Active || next.phase != GoalPhase::Paused {
                return Err(GoalProjectionError::Invalid(
                    "goal pause transition is invalid",
                ));
            }
        }
        GoalOperation::Resume => {
            require_definition(previous, next)?;
            if !matches!(
                previous.phase,
                GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
            ) || next.phase != GoalPhase::Active
                || (next.max_rounds != 0 && current.rounds_started >= next.max_rounds)
            {
                return Err(GoalProjectionError::Invalid(
                    "goal resume transition is invalid",
                ));
            }
        }
        GoalOperation::Complete => {
            require_definition(previous, next)?;
            if previous.phase == GoalPhase::Complete || next.phase != GoalPhase::Complete {
                return Err(GoalProjectionError::Invalid(
                    "goal complete transition is invalid",
                ));
            }
        }
        GoalOperation::Block => {
            require_definition(previous, next)?;
            if previous.phase != GoalPhase::Active || next.phase != GoalPhase::Blocked {
                return Err(GoalProjectionError::Invalid(
                    "goal block transition is invalid",
                ));
            }
        }
    }
    Ok(())
}

fn require_definition(
    previous: &GoalSnapshot,
    next: &GoalSnapshot,
) -> Result<(), GoalProjectionError> {
    if previous.objective != next.objective || previous.max_rounds != next.max_rounds {
        Err(GoalProjectionError::Invalid(
            "goal lifecycle mutation changed its definition",
        ))
    } else {
        Ok(())
    }
}

const WORKFLOW_CHANGE_VERSION: u8 = 1;
const MAX_WORKFLOW_ID_BYTES: usize = 128;
const MAX_WORKFLOW_NAME_BYTES: usize = 64;
const MAX_WORKFLOW_DESCRIPTION_BYTES: usize = 1024;
const MAX_WORKFLOW_STEP_ID_BYTES: usize = 64;
const MAX_WORKFLOW_STEP_LABEL_BYTES: usize = 256;
const MAX_WORKFLOW_STEPS: usize = 64;
const MAX_WORKFLOW_DELAY_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_WORKFLOW_JSON_BYTES: usize = 256 * 1024;
const MAX_WORKFLOW_JSON_DEPTH: usize = 32;
const MAX_WORKFLOW_JSON_NODES: usize = 16 * 1024;

/// Stable workflow-run identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowRunId(String);

impl WorkflowRunId {
    /// Validate externally supplied identity.
    ///
    /// # Errors
    /// Blank, oversized, or structurally unsafe ids are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, WorkflowProjectionError> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    /// Mint one fresh run id.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Borrow the opaque id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidate a deserialized run id at a trust boundary.
    ///
    /// # Errors
    /// The same identity invariants as [`Self::new`].
    pub fn validate(&self) -> Result<(), WorkflowProjectionError> {
        if valid_opaque_id(&self.0, MAX_WORKFLOW_ID_BYTES) {
            Ok(())
        } else {
            Err(WorkflowProjectionError::Invalid(
                "workflow run id is invalid",
            ))
        }
    }
}

impl std::fmt::Display for WorkflowRunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Host capability a workflow definition explicitly requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowCapability {
    /// Structured progress/checkpoint publication.
    Progress,
    /// Bounded cancellable delays.
    Delay,
    /// Guarded host tool execution.
    Tool,
    /// Registry-owned agent execution.
    Agent,
}

/// One deterministic worker action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowAction {
    /// Execute a tool through the host approval and guard chain.
    Tool {
        /// Registered tool name.
        name: String,
        /// JSON arguments; {"$ref":"step#/pointer"} binds a dependency result.
        arguments: serde_json::Value,
    },
    /// Run a fresh one-shot child through the subagent registry.
    Agent {
        /// Prompt string or a result binding resolving to a string.
        prompt: serde_json::Value,
    },
    /// Emit a plain JSON value and checkpoint immediately.
    Emit {
        /// Step result retained in the checkpoint.
        value: serde_json::Value,
    },
    /// Wait for a bounded duration, then checkpoint a JSON value.
    Delay {
        /// Positive delay no greater than 24 hours.
        millis: u64,
        /// Step result retained after the delay.
        value: serde_json::Value,
    },
}

impl Eq for WorkflowAction {}

impl WorkflowAction {
    /// Revalidate a deserialized action at a trust boundary.
    ///
    /// # Errors
    /// Delay and JSON structural bounds are enforced.
    pub fn validate(&self) -> Result<(), WorkflowProjectionError> {
        let value = match self {
            Self::Tool { name, arguments } => {
                if !valid_opaque_id(name, 128) {
                    return Err(WorkflowProjectionError::Invalid("tool name is invalid"));
                }
                arguments
            }
            Self::Agent { prompt } => {
                if !prompt
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty() && text.len() <= 64 * 1024)
                    && !prompt
                        .as_object()
                        .is_some_and(|object| object.len() == 1 && object.contains_key("$ref"))
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "agent prompt must be a nonblank bounded string or dependency binding",
                    ));
                }
                prompt
            }
            Self::Emit { value } => value,
            Self::Delay { millis, value } => {
                if *millis == 0 || *millis > MAX_WORKFLOW_DELAY_MS {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow delay is invalid",
                    ));
                }
                value
            }
        };
        validate_workflow_json(value)
    }
}

/// One ordered step of a deterministic definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase_id: Option<String>,
    id: String,
    label: String,
    action: WorkflowAction,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<serde_json::Value>,
    #[serde(default = "one_attempt", skip_serializing_if = "workflow_one_attempt")]
    max_attempts: u32,
    #[serde(default, skip_serializing_if = "workflow_false")]
    replay_safe: bool,
}

const fn workflow_one_attempt(value: &u32) -> bool {
    *value == 1
}
const fn workflow_false(value: &bool) -> bool {
    !*value
}
const fn workflow_default_parallel(value: &usize) -> bool {
    *value == 4
}

const fn one_attempt() -> u32 {
    1
}

impl WorkflowStep {
    /// Construct one validated step.
    ///
    /// # Errors
    /// Invalid id/label/action values are refused.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        action: WorkflowAction,
    ) -> Result<Self, WorkflowProjectionError> {
        let step = Self {
            phase_id: None,
            id: id.into(),
            label: label.into(),
            action,
            depends_on: Vec::new(),
            when: None,
            max_attempts: 1,
            replay_safe: false,
        };
        step.validate()?;
        Ok(step)
    }

    /// Configure this node's dependency, condition and retry policy.
    ///
    /// # Errors
    /// Invalid condition shape, dependency count or retry policy.
    pub fn with_graph_policy(
        mut self,
        depends_on: Vec<String>,
        when: Option<serde_json::Value>,
        max_attempts: u32,
        replay_safe: bool,
    ) -> Result<Self, WorkflowProjectionError> {
        self.depends_on = depends_on;
        self.when = when;
        self.max_attempts = max_attempts;
        self.replay_safe = replay_safe;
        self.validate()?;
        Ok(self)
    }

    /// Optional explicitly declared presentation phase. Does not alter dependencies.
    pub fn phase_id(&self) -> Option<&str> {
        self.phase_id.as_deref()
    }

    /// Assign a declared phase. Definition validation checks its existence.
    pub fn with_phase(
        mut self,
        phase_id: impl Into<String>,
    ) -> Result<Self, WorkflowProjectionError> {
        self.phase_id = Some(phase_id.into());
        self.validate()?;
        Ok(self)
    }

    /// Stable step id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Observer label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Deterministic worker action.
    #[must_use]
    pub const fn action(&self) -> &WorkflowAction {
        &self.action
    }

    /// Explicit dependencies (graph definitions only).
    pub fn depends_on(&self) -> &[String] {
        &self.depends_on
    }
    /// Optional boolean or bound boolean gating execution.
    pub fn when(&self) -> Option<&serde_json::Value> {
        self.when.as_ref()
    }
    /// Maximum total attempts, including resumed attempts.
    pub const fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
    /// Explicit caller assertion permitting retries of failed effects.
    pub const fn replay_safe(&self) -> bool {
        self.replay_safe
    }

    fn validate(&self) -> Result<(), WorkflowProjectionError> {
        if self
            .phase_id
            .as_ref()
            .is_some_and(|id| !valid_opaque_id(id, MAX_WORKFLOW_STEP_ID_BYTES))
        {
            return Err(WorkflowProjectionError::Invalid(
                "invalid workflow phase id",
            ));
        }
        if !valid_opaque_id(&self.id, MAX_WORKFLOW_STEP_ID_BYTES)
            || !valid_text(&self.label, MAX_WORKFLOW_STEP_LABEL_BYTES)
        {
            return Err(WorkflowProjectionError::Invalid(
                "workflow step identity is invalid",
            ));
        }
        if !(1..=5).contains(&self.max_attempts) || self.depends_on.len() > 64 {
            return Err(WorkflowProjectionError::Invalid(
                "max_attempts must be 1..5; at most 64 dependencies",
            ));
        }
        if self.max_attempts > 1
            && !self.replay_safe
            && matches!(
                self.action,
                WorkflowAction::Tool { .. } | WorkflowAction::Agent { .. }
            )
        {
            return Err(WorkflowProjectionError::Invalid(
                "effect retries require replay_safe=true",
            ));
        }
        if let Some(when) = &self.when {
            if !when.is_boolean()
                && !when.as_object().is_some_and(|object| {
                    object.len() == 1
                        && (object.contains_key("$ref")
                            || object
                                .get("equals")
                                .and_then(serde_json::Value::as_array)
                                .is_some_and(|values| values.len() == 2))
                })
            {
                return Err(WorkflowProjectionError::Invalid(
                    "when must be a boolean, dependency binding, or {equals:[left,right]}",
                ));
            }
            validate_workflow_json(when)?;
        }
        self.action.validate()
    }
}

/// Version-one deterministic workflow definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    phases: Vec<WorkflowPhase>,
    version: u8,
    name: String,
    description: String,
    capabilities: Vec<WorkflowCapability>,
    steps: Vec<WorkflowStep>,
    #[serde(
        default = "default_parallel",
        skip_serializing_if = "workflow_default_parallel"
    )]
    max_parallel: usize,
}

const fn default_parallel() -> usize {
    4
}

impl WorkflowDefinition {
    /// Construct one validated definition.
    ///
    /// # Errors
    /// Empty/duplicate/unbounded fields or an action missing its declared host
    /// capability are refused.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        capabilities: Vec<WorkflowCapability>,
        steps: Vec<WorkflowStep>,
    ) -> Result<Self, WorkflowProjectionError> {
        let definition = Self {
            title: None,
            phases: Vec::new(),
            version: WORKFLOW_CHANGE_VERSION,
            name: name.into(),
            description: description.into(),
            capabilities,
            steps,
            max_parallel: default_parallel(),
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Construct a native version-two graph definition.
    ///
    /// # Errors
    /// Invalid metadata, dependencies, bindings, cycles or execution bounds.
    pub fn graph(
        name: impl Into<String>,
        description: impl Into<String>,
        capabilities: Vec<WorkflowCapability>,
        steps: Vec<WorkflowStep>,
        max_parallel: usize,
    ) -> Result<Self, WorkflowProjectionError> {
        let definition = Self {
            title: None,
            phases: Vec::new(),
            version: 2,
            name: name.into(),
            description: description.into(),
            capabilities,
            steps,
            max_parallel,
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Explicit human title, falling back to the exact declared definition name.
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }
    /// Explicit phases in planned order. Empty means each declared step is a phase.
    pub fn phases(&self) -> &[WorkflowPhase] {
        &self.phases
    }
    /// Attach explicit presentation metadata without changing execution order.
    pub fn with_presentation(
        mut self,
        title: impl Into<String>,
        phases: Vec<WorkflowPhase>,
    ) -> Result<Self, WorkflowProjectionError> {
        self.title = Some(title.into());
        self.phases = phases;
        self.validate()?;
        Ok(self)
    }

    /// Whether this definition uses dependency graph execution (version 2).
    pub const fn is_graph(&self) -> bool {
        self.version == 2
    }
    /// Bound on simultaneous graph nodes.
    pub const fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// Definition name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human-readable purpose.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Explicit required host capabilities.
    #[must_use]
    pub fn capabilities(&self) -> &[WorkflowCapability] {
        &self.capabilities
    }

    /// Ordered deterministic steps.
    #[must_use]
    pub fn steps(&self) -> &[WorkflowStep] {
        &self.steps
    }

    /// Revalidate a deserialized definition at a trust boundary.
    ///
    /// # Errors
    /// The same schema/capability/size invariants as [`Self::new`].
    pub fn validate(&self) -> Result<(), WorkflowProjectionError> {
        if self
            .title
            .as_ref()
            .is_some_and(|title| !valid_text(title, MAX_WORKFLOW_STEP_LABEL_BYTES))
            || self.phases.len() > MAX_WORKFLOW_STEPS
        {
            return Err(WorkflowProjectionError::Invalid(
                "invalid workflow title or phase count",
            ));
        }
        let mut phase_ids = BTreeSet::new();
        for phase in &self.phases {
            if !valid_opaque_id(&phase.id, MAX_WORKFLOW_STEP_ID_BYTES)
                || !valid_text(&phase.title, MAX_WORKFLOW_STEP_LABEL_BYTES)
                || !phase_ids.insert(phase.id.as_str())
            {
                return Err(WorkflowProjectionError::Invalid(
                    "phase ids must be unique and titles valid",
                ));
            }
        }
        for step in &self.steps {
            if (self.phases.is_empty() && step.phase_id.is_some())
                || (!self.phases.is_empty()
                    && !step
                        .phase_id
                        .as_deref()
                        .is_some_and(|id| phase_ids.contains(id)))
            {
                return Err(WorkflowProjectionError::Invalid(
                    "every step must reference a declared phase when phases are supplied",
                ));
            }
        }
        if self.phases.iter().any(|phase| {
            !self
                .steps
                .iter()
                .any(|step| step.phase_id.as_deref() == Some(phase.id.as_str()))
        }) {
            return Err(WorkflowProjectionError::Invalid(
                "declared phases must contain at least one step",
            ));
        }
        if ![1, 2].contains(&self.version) {
            return Err(WorkflowProjectionError::Invalid(
                "version must be 1 (legacy) or 2 (native graph)",
            ));
        }
        if !(1..=8).contains(&self.max_parallel) {
            return Err(WorkflowProjectionError::Invalid(
                "max_parallel must be 1..8",
            ));
        }
        if !valid_opaque_id(&self.name, MAX_WORKFLOW_NAME_BYTES) {
            return Err(WorkflowProjectionError::Invalid(
                "name must be 1..64 ASCII letters, digits, underscore, hyphen, dot or colon",
            ));
        }
        if !valid_text(&self.description, MAX_WORKFLOW_DESCRIPTION_BYTES) {
            return Err(WorkflowProjectionError::Invalid(
                "description must be nonblank safe text of at most 1024 bytes",
            ));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_WORKFLOW_STEPS {
            return Err(WorkflowProjectionError::Invalid(
                "steps must contain 1..64 nodes",
            ));
        }
        let capabilities = self.capabilities.iter().copied().collect::<BTreeSet<_>>();
        if capabilities.len() != self.capabilities.len()
            || !capabilities.contains(&WorkflowCapability::Progress)
        {
            return Err(WorkflowProjectionError::Invalid(
                "workflow capabilities are invalid",
            ));
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            step.validate()?;
            if !ids.insert(step.id.as_str()) {
                return Err(WorkflowProjectionError::Invalid(
                    "workflow step ids are duplicated",
                ));
            }
            let required = match step.action {
                WorkflowAction::Delay { .. } => WorkflowCapability::Delay,
                WorkflowAction::Tool { .. } => WorkflowCapability::Tool,
                WorkflowAction::Agent { .. } => WorkflowCapability::Agent,
                WorkflowAction::Emit { .. } => WorkflowCapability::Progress,
            };
            if !capabilities.contains(&required) {
                return Err(WorkflowProjectionError::Invalid(
                    "workflow action lacks its declared capability",
                ));
            }
        }
        for step in &self.steps {
            if !self.is_graph()
                && (!step.depends_on.is_empty()
                    || step.when.is_some()
                    || step.max_attempts != 1
                    || matches!(
                        step.action,
                        WorkflowAction::Tool { .. } | WorkflowAction::Agent { .. }
                    ))
            {
                return Err(WorkflowProjectionError::Invalid(
                    "agent/tool actions and dependencies require version 2",
                ));
            }
            let deps = step.depends_on.iter().collect::<BTreeSet<_>>();
            if deps.len() != step.depends_on.len()
                || deps
                    .iter()
                    .any(|id| !ids.contains(id.as_str()) || id.as_str() == step.id)
            {
                return Err(WorkflowProjectionError::Invalid(
                    "dependencies must be unique existing other step ids",
                ));
            }
            let action = serde_json::to_value(&step.action)
                .map_err(|_| WorkflowProjectionError::Invalid("action JSON is invalid"))?;
            validate_bindings(&action, &step.depends_on)?;
            if let Some(when) = &step.when {
                validate_bindings(when, &step.depends_on)?;
            }
        }
        let mut ready = BTreeSet::new();
        loop {
            let before = ready.len();
            for step in &self.steps {
                if step.depends_on.iter().all(|id| ready.contains(id.as_str())) {
                    ready.insert(step.id.as_str());
                }
            }
            if ready.len() == self.steps.len() {
                break;
            }
            if before == ready.len() {
                return Err(WorkflowProjectionError::Invalid(
                    "dependency graph contains a cycle",
                ));
            }
        }
        Ok(())
    }
}

fn validate_bindings(
    value: &serde_json::Value,
    dependencies: &[String],
) -> Result<(), WorkflowProjectionError> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                let reference = reference
                    .as_str()
                    .ok_or(WorkflowProjectionError::Invalid("$ref must be a string"))?;
                let (id, pointer) =
                    reference
                        .split_once('#')
                        .ok_or(WorkflowProjectionError::Invalid(
                            "$ref must be step#/json/pointer",
                        ))?;
                if object.len() != 1
                    || !dependencies.iter().any(|dep| dep == id)
                    || (!pointer.is_empty() && !pointer.starts_with('/'))
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "$ref must name a declared dependency and JSON pointer",
                    ));
                }
            } else {
                for value in object.values() {
                    validate_bindings(value, dependencies)?;
                }
            }
        }
        serde_json::Value::Array(array) => {
            for value in array {
                validate_bindings(value, dependencies)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Durable lifecycle for an individual graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeState {
    /// Intent flushed before any effect. Unsettled intents require reconciliation.
    Started,
    /// Result durably committed.
    Completed,
    /// Condition was false or a dependency was skipped.
    Skipped,
    /// Attempt failed; retry requires an explicit replay safety declaration.
    Failed,
}

/// Latest durable attempt and result for one graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNodeRecord {
    /// Total attempt number.
    pub attempt: u32,
    /// Durable lifecycle.
    pub state: WorkflowNodeState,
    /// Result or bounded failure description.
    pub value: serde_json::Value,
}

/// Durable terminal outcome of one workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcome {
    /// Stopped at a committed step boundary and may resume.
    Paused,
    /// Every step checkpointed.
    Completed,
    /// Worker or durable progress failed.
    Failed,
    /// The one run cancellation token stopped execution.
    Cancelled,
}

/// Version-one workflow lifecycle mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowChange {
    /// Host-authored coordinator admission, flushed before executing this attempt.
    Job {
        /// Payload version.
        version: u8,
        /// Owning run.
        run_id: WorkflowRunId,
        /// Exact current run attempt.
        attempt: u32,
        /// Actual canonical JobRegistry ID.
        job_id: String,
    },
    /// Host-authored native task identity and observation for an admitted graph node.
    Agent {
        /// Payload version.
        version: u8,
        /// Owning workflow run.
        run_id: WorkflowRunId,
        /// Exact native task observation.
        record: WorkflowAgentRecord,
    },
    /// Save or replace a reusable definition in this session's library.
    Saved {
        /// Payload version.
        version: u8,
        /// Definition name is the stable library key. Runs retain their own copy.
        definition: WorkflowDefinition,
    },
    /// Graph node intent or settlement, committed before dependent execution.
    Node {
        /// Payload version.
        version: u8,
        /// Owning run.
        run_id: WorkflowRunId,
        /// Definition step id.
        step_id: String,
        /// Attempt and result.
        record: WorkflowNodeRecord,
    },
    /// A validated definition began its first attempt.
    Start {
        /// Payload version.
        version: u8,
        /// Stable run id.
        run_id: WorkflowRunId,
        /// Complete immutable definition.
        definition: WorkflowDefinition,
    },
    /// One observer-safe progress item.
    Progress {
        /// Payload version.
        version: u8,
        /// Stable run id.
        run_id: WorkflowRunId,
        /// Contiguous progress sequence across attempts.
        sequence: u32,
        /// One-based step being executed.
        step: u32,
        /// Bounded observer narration.
        message: String,
    },
    /// Durable result after one completed step.
    Checkpoint {
        /// Payload version.
        version: u8,
        /// Stable run id.
        run_id: WorkflowRunId,
        /// Number of completed prefix steps.
        completed_steps: u32,
        /// Plain bounded JSON result for the latest step.
        value: serde_json::Value,
    },
    /// Explicit restart from the latest durable checkpoint.
    Resume {
        /// Payload version.
        version: u8,
        /// Stable run id.
        run_id: WorkflowRunId,
        /// Positive contiguous attempt number.
        attempt: u32,
        /// Exact checkpoint prefix resumed.
        completed_steps: u32,
    },
    /// Terminal settlement.
    End {
        /// Payload version.
        version: u8,
        /// Stable run id.
        run_id: WorkflowRunId,
        /// Closed outcome.
        outcome: WorkflowOutcome,
        /// Completed prefix at settlement.
        completed_steps: u32,
        /// Optional bounded safe failure detail.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl WorkflowChange {
    /// Start attempt one.
    #[must_use]
    pub const fn start(run_id: WorkflowRunId, definition: WorkflowDefinition) -> Self {
        Self::Start {
            version: WORKFLOW_CHANGE_VERSION,
            run_id,
            definition,
        }
    }

    /// Construct one progress item.
    ///
    /// # Errors
    /// Zero sequence/step or invalid text is refused.
    pub fn progress(
        run_id: WorkflowRunId,
        sequence: u32,
        step: u32,
        message: impl Into<String>,
    ) -> Result<Self, WorkflowProjectionError> {
        let change = Self::Progress {
            version: WORKFLOW_CHANGE_VERSION,
            run_id,
            sequence,
            step,
            message: message.into(),
        };
        change.validate_shape()?;
        Ok(change)
    }

    /// Construct a step checkpoint.
    ///
    /// # Errors
    /// Zero prefix or unbounded/non-finite JSON is refused.
    pub fn checkpoint(
        run_id: WorkflowRunId,
        completed_steps: u32,
        value: serde_json::Value,
    ) -> Result<Self, WorkflowProjectionError> {
        let change = Self::Checkpoint {
            version: WORKFLOW_CHANGE_VERSION,
            run_id,
            completed_steps,
            value,
        };
        change.validate_shape()?;
        Ok(change)
    }

    /// Construct an explicit resume attempt.
    ///
    /// # Errors
    /// Attempts below two are refused.
    pub fn resume(
        run_id: WorkflowRunId,
        attempt: u32,
        completed_steps: u32,
    ) -> Result<Self, WorkflowProjectionError> {
        let change = Self::Resume {
            version: WORKFLOW_CHANGE_VERSION,
            run_id,
            attempt,
            completed_steps,
        };
        change.validate_shape()?;
        Ok(change)
    }

    /// Construct terminal settlement.
    ///
    /// # Errors
    /// Outcome/message coherence or bounded text violations are refused.
    pub fn end(
        run_id: WorkflowRunId,
        outcome: WorkflowOutcome,
        completed_steps: u32,
        message: Option<String>,
    ) -> Result<Self, WorkflowProjectionError> {
        let change = Self::End {
            version: WORKFLOW_CHANGE_VERSION,
            run_id,
            outcome,
            completed_steps,
            message,
        };
        change.validate_shape()?;
        Ok(change)
    }

    pub(crate) fn validate_shape(&self) -> Result<(), WorkflowProjectionError> {
        if let Self::Saved {
            version,
            definition,
        } = self
        {
            if *version != WORKFLOW_CHANGE_VERSION {
                return Err(WorkflowProjectionError::Invalid(
                    "unsupported saved workflow version",
                ));
            }
            return definition.validate();
        }
        let (version, run_id) = match self {
            Self::Saved { .. } => {
                return Err(WorkflowProjectionError::Invalid("invalid saved workflow"));
            }
            Self::Job {
                version, run_id, ..
            }
            | Self::Agent {
                version, run_id, ..
            }
            | Self::Node {
                version, run_id, ..
            }
            | Self::Start {
                version, run_id, ..
            }
            | Self::Progress {
                version, run_id, ..
            }
            | Self::Checkpoint {
                version, run_id, ..
            }
            | Self::Resume {
                version, run_id, ..
            }
            | Self::End {
                version, run_id, ..
            } => (*version, run_id),
        };
        if version != WORKFLOW_CHANGE_VERSION {
            return Err(WorkflowProjectionError::Invalid(
                "workflow change version is unsupported",
            ));
        }
        run_id.validate()?;
        match self {
            Self::Saved { definition, .. } => definition.validate(),
            Self::Job {
                attempt, job_id, ..
            } => {
                if *attempt == 0
                    || job_id.len() > 24
                    || !job_id
                        .strip_prefix("job-")
                        .and_then(|number| number.parse::<u64>().ok())
                        .is_some_and(|number| format!("job-{number}") == *job_id)
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "invalid workflow coordinator identity or attempt",
                    ));
                }
                Ok(())
            }
            Self::Agent { record, .. } => {
                if !valid_opaque_id(&record.node_id, MAX_WORKFLOW_STEP_ID_BYTES)
                    || !(1..=5).contains(&record.node_attempt)
                    || !valid_opaque_id(&record.task_id, 128)
                    || !valid_opaque_id(&record.owner_session_id, 128)
                    || record
                        .session_id
                        .as_ref()
                        .is_some_and(|id| !valid_opaque_id(id, 128))
                    || record
                        .job_id
                        .as_ref()
                        .is_some_and(|id| !valid_opaque_id(id, 128))
                    || !valid_text(&record.label, MAX_WORKFLOW_STEP_LABEL_BYTES)
                    || record.prompt.is_empty()
                    || record.prompt.len() > 16 * 1024
                    || record.summary.len() > 32 * 1024
                    || record.created_at_ms < 0
                    || record.updated_at_ms < record.created_at_ms
                    || record.revision == 0
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "invalid workflow agent observation",
                    ));
                }
                Ok(())
            }
            Self::Node {
                step_id, record, ..
            } => {
                if !valid_opaque_id(step_id, MAX_WORKFLOW_STEP_ID_BYTES)
                    || !(1..=5).contains(&record.attempt)
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "invalid graph node identity or attempt",
                    ));
                }
                validate_workflow_json(&record.value)
            }
            Self::Start { definition, .. } => definition.validate(),
            Self::Progress {
                sequence,
                step,
                message,
                ..
            } => {
                if *sequence == 0
                    || *step == 0
                    || !valid_text(message, MAX_WORKFLOW_STEP_LABEL_BYTES)
                {
                    Err(WorkflowProjectionError::Invalid(
                        "workflow progress is invalid",
                    ))
                } else {
                    Ok(())
                }
            }
            Self::Checkpoint {
                completed_steps,
                value,
                ..
            } => {
                if *completed_steps == 0 {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow checkpoint prefix is invalid",
                    ));
                }
                validate_workflow_json(value)
            }
            Self::Resume { attempt, .. } if *attempt < 2 => Err(WorkflowProjectionError::Invalid(
                "workflow resume attempt is invalid",
            )),
            Self::Resume { .. } => Ok(()),
            Self::End {
                outcome, message, ..
            } => {
                let coherent = (*outcome == WorkflowOutcome::Completed && message.is_none())
                    || (*outcome != WorkflowOutcome::Completed
                        && message.as_ref().is_some_and(|message| {
                            valid_text(message, MAX_WORKFLOW_DESCRIPTION_BYTES)
                        }));
                if coherent {
                    Ok(())
                } else {
                    Err(WorkflowProjectionError::Invalid(
                        "workflow settlement is invalid",
                    ))
                }
            }
        }
    }
}

/// Durable state of a workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowState {
    /// Explicitly paused at a safe checkpoint boundary.
    Paused,
    /// No terminal event; it can resume from its checkpoint after restart.
    Running,
    /// Every step completed.
    Completed,
    /// Run failed.
    Failed,
    /// Run was cancelled.
    Cancelled,
}

/// Strict replay view of one workflow run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunProjection {
    jobs: BTreeMap<u32, WorkflowJobRecord>,
    attempt_activity: bool,
    agents: std::collections::BTreeMap<String, WorkflowAgentRecord>,
    definition: WorkflowDefinition,
    state: WorkflowState,
    attempt: u32,
    progress_sequence: u32,
    completed_steps: u32,
    checkpoint: Option<serde_json::Value>,
    nodes: std::collections::BTreeMap<String, WorkflowNodeRecord>,
}

impl WorkflowRunProjection {
    /// Exact coordinator admissions and settled outcomes in workflow attempt order.
    pub fn jobs(&self) -> &BTreeMap<u32, WorkflowJobRecord> {
        &self.jobs
    }
    /// Exact native tasks correlated to node attempts, retained after handles close.
    pub fn agents(&self) -> &std::collections::BTreeMap<String, WorkflowAgentRecord> {
        &self.agents
    }
    /// Authoritative node attempts/results, including crash-left intents.
    pub fn nodes(&self) -> &std::collections::BTreeMap<String, WorkflowNodeRecord> {
        &self.nodes
    }

    /// Immutable definition.
    #[must_use]
    pub const fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    /// Current durable state.
    #[must_use]
    pub const fn state(&self) -> WorkflowState {
        self.state
    }

    /// Current attempt number.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Latest contiguous progress sequence across attempts.
    #[must_use]
    pub const fn progress_sequence(&self) -> u32 {
        self.progress_sequence
    }

    /// Completed prefix length.
    #[must_use]
    pub const fn completed_steps(&self) -> u32 {
        self.completed_steps
    }

    /// Latest step checkpoint value.
    #[must_use]
    pub const fn checkpoint(&self) -> Option<&serde_json::Value> {
        self.checkpoint.as_ref()
    }
}

/// Ordered replay of every workflow id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowProjection {
    runs: std::collections::BTreeMap<WorkflowRunId, WorkflowRunProjection>,
    definitions: std::collections::BTreeMap<String, WorkflowDefinition>,
}

impl WorkflowProjection {
    /// Reusable definitions in stable name order, as last saved in this session.
    pub fn definitions(&self) -> &std::collections::BTreeMap<String, WorkflowDefinition> {
        &self.definitions
    }

    /// Find one run.
    #[must_use]
    pub fn get(&self, id: &WorkflowRunId) -> Option<&WorkflowRunProjection> {
        self.runs.get(id)
    }

    /// Runs in stable id order.
    pub fn iter(&self) -> impl Iterator<Item = (&WorkflowRunId, &WorkflowRunProjection)> {
        self.runs.iter()
    }
}

/// Strict workflow replay failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowProjectionError {
    /// One bounded shape or transition invariant failed.
    #[error("invalid durable workflow state: {0}")]
    Invalid(&'static str),
}

/// Rebuild all workflow runs from durable events.
///
/// # Errors
/// Invalid schemas, duplicate ids, non-contiguous progress/attempts/checkpoints,
/// or updates after terminal settlement fail loud.
pub fn project_workflows(
    events: &[SessionEvent],
) -> Result<WorkflowProjection, WorkflowProjectionError> {
    let mut projection = WorkflowProjection::default();
    for event in events {
        let SessionEventKind::WorkflowChange { change } = &event.kind else {
            continue;
        };
        change.validate_shape()?;
        match change.as_ref() {
            WorkflowChange::Job {
                run_id,
                attempt,
                job_id,
                ..
            } => {
                if projection
                    .runs
                    .values()
                    .any(|run| run.jobs.values().any(|job| job.job_id == *job_id))
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow coordinator job id reused",
                    ));
                }
                let run = running_workflow(&mut projection, run_id)?;
                if run.attempt != *attempt || run.jobs.contains_key(attempt) || run.attempt_activity
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow coordinator must be recorded once before attempt work",
                    ));
                }
                run.jobs.insert(
                    *attempt,
                    WorkflowJobRecord {
                        attempt: *attempt,
                        job_id: job_id.clone(),
                        admitted_at_ms: event.time_ms,
                        settled_at_ms: None,
                        outcome: None,
                    },
                );
            }

            WorkflowChange::Agent { run_id, record, .. } => {
                if projection
                    .runs
                    .iter()
                    .any(|(id, run)| id != run_id && run.agents.contains_key(&record.task_id))
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow task identity reused across runs",
                    ));
                }
                let run =
                    projection
                        .runs
                        .get_mut(run_id)
                        .ok_or(WorkflowProjectionError::Invalid(
                            "agent observation has no workflow run",
                        ))?;
                let step = run
                    .definition
                    .steps
                    .iter()
                    .find(|step| step.id == record.node_id)
                    .ok_or(WorkflowProjectionError::Invalid(
                        "agent observation has unknown node",
                    ))?;
                if !matches!(step.action, WorkflowAction::Agent { .. })
                    || !run
                        .nodes
                        .get(&record.node_id)
                        .is_some_and(|node| node.attempt == record.node_attempt)
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "agent observation does not match agent node attempt",
                    ));
                }
                if let Some(prior) = run.agents.get(&record.task_id) {
                    if prior.node_id != record.node_id
                        || prior.node_attempt != record.node_attempt
                        || prior.owner_session_id != record.owner_session_id
                        || prior.label != record.label
                        || prior.prompt != record.prompt
                        || prior.prompt_truncated != record.prompt_truncated
                        || prior.created_at_ms != record.created_at_ms
                        || prior.revision >= record.revision
                        || prior.updated_at_ms > record.updated_at_ms
                        || prior
                            .session_id
                            .as_ref()
                            .is_some_and(|id| record.session_id.as_ref() != Some(id))
                        || prior
                            .job_id
                            .as_ref()
                            .is_some_and(|id| record.job_id.as_ref() != Some(id))
                        || prior.usage.reported_requests > record.usage.reported_requests
                        || prior.usage.unreported_requests > record.usage.unreported_requests
                        || prior.usage.prompt_tokens > record.usage.prompt_tokens
                        || prior.usage.completion_tokens > record.usage.completion_tokens
                    {
                        return Err(WorkflowProjectionError::Invalid(
                            "workflow task correlation changed or observation regressed",
                        ));
                    }
                } else if run.state != WorkflowState::Running
                    || !run
                        .nodes
                        .get(&record.node_id)
                        .is_some_and(|node| node.state == WorkflowNodeState::Started)
                    || run.agents.values().any(|prior| {
                        prior.node_id == record.node_id && prior.node_attempt == record.node_attempt
                    })
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow task must link once before node settlement",
                    ));
                }
                run.agents.insert(record.task_id.clone(), record.clone());
            }
            WorkflowChange::Saved { definition, .. } => {
                if !projection.definitions.contains_key(definition.name())
                    && projection.definitions.len() >= 64
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "saved workflow library is full (64 definitions)",
                    ));
                }
                projection
                    .definitions
                    .insert(definition.name().to_owned(), definition.clone());
            }
            WorkflowChange::Node {
                run_id,
                step_id,
                record,
                ..
            } => {
                let run = running_workflow(&mut projection, run_id)?;
                let step = run
                    .definition
                    .steps
                    .iter()
                    .find(|step| step.id == *step_id)
                    .ok_or(WorkflowProjectionError::Invalid("unknown graph node"))?;
                if !run.definition.is_graph() || record.attempt > step.max_attempts {
                    return Err(WorkflowProjectionError::Invalid(
                        "graph node attempt exceeds definition",
                    ));
                }
                let prior = run.nodes.get(step_id);
                let valid = match record.state {
                    WorkflowNodeState::Started => prior.map_or(record.attempt == 1, |prior| {
                        prior.state == WorkflowNodeState::Failed
                            && record.attempt == prior.attempt + 1
                            && step.replay_safe
                    }),
                    WorkflowNodeState::Skipped => prior.is_none() && record.attempt == 1,
                    WorkflowNodeState::Completed | WorkflowNodeState::Failed => {
                        prior.is_some_and(|prior| {
                            prior.state == WorkflowNodeState::Started
                                && prior.attempt == record.attempt
                        })
                    }
                };
                if !valid
                    || step.depends_on.iter().any(|id| {
                        !run.nodes.get(id).is_some_and(|node| {
                            matches!(
                                node.state,
                                WorkflowNodeState::Completed | WorkflowNodeState::Skipped
                            )
                        })
                    })
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "invalid graph node transition or unsettled dependencies",
                    ));
                }
                run.attempt_activity = true;
                run.nodes.insert(step_id.clone(), record.clone());
                run.completed_steps = run
                    .nodes
                    .values()
                    .filter(|node| {
                        matches!(
                            node.state,
                            WorkflowNodeState::Completed | WorkflowNodeState::Skipped
                        )
                    })
                    .count() as u32;
                run.checkpoint = Some(record.value.clone());
            }
            WorkflowChange::Start {
                run_id, definition, ..
            } => {
                if projection.runs.contains_key(run_id) {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow run id was reused",
                    ));
                }
                projection.runs.insert(
                    run_id.clone(),
                    WorkflowRunProjection {
                        jobs: BTreeMap::new(),
                        attempt_activity: false,
                        agents: std::collections::BTreeMap::new(),
                        definition: definition.clone(),
                        state: WorkflowState::Running,
                        attempt: 1,
                        progress_sequence: 0,
                        completed_steps: 0,
                        checkpoint: None,
                        nodes: std::collections::BTreeMap::new(),
                    },
                );
            }
            WorkflowChange::Progress {
                run_id,
                sequence,
                step,
                ..
            } => {
                let run = running_workflow(&mut projection, run_id)?;
                if run.definition.is_graph()
                    || *sequence != run.progress_sequence.saturating_add(1)
                    || *step != run.completed_steps.saturating_add(1)
                    || usize::try_from(*step).map_or(true, |step| step > run.definition.steps.len())
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow progress is non-contiguous",
                    ));
                }
                run.attempt_activity = true;
                run.progress_sequence = *sequence;
            }
            WorkflowChange::Checkpoint {
                run_id,
                completed_steps,
                value,
                ..
            } => {
                let run = running_workflow(&mut projection, run_id)?;
                if run.definition.is_graph()
                    || *completed_steps != run.completed_steps.saturating_add(1)
                    || *completed_steps > run.progress_sequence
                    || usize::try_from(*completed_steps)
                        .map_or(true, |steps| steps > run.definition.steps.len())
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow checkpoint skipped a step",
                    ));
                }
                run.completed_steps = *completed_steps;
                run.checkpoint = Some(value.clone());
            }
            WorkflowChange::Resume {
                run_id,
                attempt,
                completed_steps,
                ..
            } => {
                let run = projection
                    .runs
                    .get_mut(run_id)
                    .ok_or(WorkflowProjectionError::Invalid("unknown workflow run"))?;
                if !matches!(run.state, WorkflowState::Running | WorkflowState::Paused) {
                    return Err(WorkflowProjectionError::Invalid("workflow is terminal"));
                }
                if *attempt != run.attempt.saturating_add(1)
                    || *completed_steps != run.completed_steps
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow resume does not match its checkpoint",
                    ));
                }
                run.attempt = *attempt;
                run.attempt_activity = false;
                run.state = WorkflowState::Running;
            }
            WorkflowChange::End {
                run_id,
                outcome,
                completed_steps,
                ..
            } => {
                let run = projection
                    .runs
                    .get_mut(run_id)
                    .ok_or(WorkflowProjectionError::Invalid("unknown workflow run"))?;
                if run.state != WorkflowState::Running
                    && !(run.state == WorkflowState::Paused
                        && *outcome == WorkflowOutcome::Cancelled)
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow update follows terminal settlement",
                    ));
                }
                if *completed_steps != run.completed_steps
                    || (*outcome == WorkflowOutcome::Paused
                        && run
                            .nodes
                            .values()
                            .any(|node| node.state == WorkflowNodeState::Started))
                    || (*outcome == WorkflowOutcome::Completed
                        && usize::try_from(*completed_steps) != Ok(run.definition.steps.len()))
                {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow settlement does not match its checkpoint",
                    ));
                }
                if let Some(job) = run.jobs.get_mut(&run.attempt)
                    && job.outcome.is_none()
                {
                    job.outcome = Some(*outcome);
                    job.settled_at_ms = Some(event.time_ms);
                }
                run.state = match outcome {
                    WorkflowOutcome::Paused => WorkflowState::Paused,
                    WorkflowOutcome::Completed => WorkflowState::Completed,
                    WorkflowOutcome::Failed => WorkflowState::Failed,
                    WorkflowOutcome::Cancelled => WorkflowState::Cancelled,
                };
            }
        }
    }
    Ok(projection)
}

fn running_workflow<'a>(
    projection: &'a mut WorkflowProjection,
    id: &WorkflowRunId,
) -> Result<&'a mut WorkflowRunProjection, WorkflowProjectionError> {
    let run = projection
        .runs
        .get_mut(id)
        .ok_or(WorkflowProjectionError::Invalid(
            "workflow update has no start",
        ))?;
    if run.state != WorkflowState::Running {
        return Err(WorkflowProjectionError::Invalid(
            "workflow update follows terminal settlement",
        ));
    }
    Ok(run)
}

fn validate_workflow_json(value: &serde_json::Value) -> Result<(), WorkflowProjectionError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| WorkflowProjectionError::Invalid("workflow JSON is invalid"))?;
    if bytes.len() > MAX_WORKFLOW_JSON_BYTES {
        return Err(WorkflowProjectionError::Invalid(
            "workflow JSON exceeds its byte bound",
        ));
    }
    let mut nodes = 0_usize;
    validate_workflow_json_node(value, 0, &mut nodes)
}

fn validate_workflow_json_node(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), WorkflowProjectionError> {
    *nodes = nodes.saturating_add(1);
    if depth > MAX_WORKFLOW_JSON_DEPTH || *nodes > MAX_WORKFLOW_JSON_NODES {
        return Err(WorkflowProjectionError::Invalid(
            "workflow JSON exceeds its structural bound",
        ));
    }
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                validate_workflow_json_node(value, depth.saturating_add(1), nodes)?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if !valid_text(key, MAX_WORKFLOW_STEP_LABEL_BYTES) {
                    return Err(WorkflowProjectionError::Invalid(
                        "workflow JSON object key is invalid",
                    ));
                }
                validate_workflow_json_node(value, depth.saturating_add(1), nodes)?;
            }
        }
        serde_json::Value::Number(number) if number.as_f64().is_some_and(f64::is_infinite) => {
            return Err(WorkflowProjectionError::Invalid(
                "workflow JSON number is non-finite",
            ));
        }
        _ => {}
    }
    Ok(())
}

const LEGACY_SCHEDULE_CHANGE_VERSION: u8 = 1;
const SCHEDULE_CHANGE_VERSION: u8 = 2;
const MAX_SCHEDULE_ID_BYTES: usize = 128;
const MAX_SCHEDULE_PROMPT_BYTES: usize = 64 * 1024;
const MAX_SCHEDULE_DELAY_MS: u64 = 365 * 24 * 60 * 60 * 1000;
const MIN_SCHEDULE_EVERY_MS: u64 = 1_000;
const MIN_WAKEUP_DELAY_MS: u64 = 60 * 1_000;
const MAX_WAKEUP_DELAY_MS: u64 = 60 * 60 * 1_000;
const SCHEDULE_EXPIRY_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const RECURRING_JITTER_MAX_MS: i64 = 30 * 60 * 1_000;
const ONE_SHOT_JITTER_MAX_MS: i64 = 90 * 1_000;
const CRON_SEARCH_DAYS: usize = 8 * 366;

/// Maximum live records, including a self-paced loop awaiting reschedule.
pub const MAX_ACTIVE_SCHEDULES: usize = 50;

/// Stable schedule identity unique within one local session suffix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduleId(String);

impl ScheduleId {
    /// Validate externally supplied identity.
    ///
    /// # Errors
    /// Blank, oversized, or unsafe identity text is refused.
    pub fn new(value: impl Into<String>) -> Result<Self, ScheduleProjectionError> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    /// Mint one fresh session-local identity.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Mint the compact identity shape used by native cron-compatible records.
    #[must_use]
    pub fn generate_short() -> Self {
        let value = uuid::Uuid::new_v4().simple().to_string();
        Self(value[..8].to_owned())
    }

    /// Borrow the opaque identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), ScheduleProjectionError> {
        if valid_opaque_id(&self.0, MAX_SCHEDULE_ID_BYTES) {
            Ok(())
        } else {
            Err(ScheduleProjectionError::Invalid("schedule id is invalid"))
        }
    }
}

impl std::fmt::Display for ScheduleId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Time interpretation retained by a cron schedule.
///
/// Claude's session cron contract has no timezone selector: expressions are
/// interpreted by the machine running the session. Keeping that fact in the
/// durable record prevents the tool surface from implying UTC or hosted time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleTimeZone {
    /// The operating system's current local timezone.
    Local,
}

/// Timing rule retained on one active schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRule {
    /// One-shot target derived from a positive delay at creation.
    After {
        /// Original positive delay.
        delay_ms: u64,
    },
    /// One-shot absolute Unix-millisecond target.
    At,
    /// Creation-anchor-aligned fixed-rate recurrence.
    Every {
        /// Positive recurrence interval.
        every_ms: u64,
    },
    /// Standard five-field cron expression evaluated in local time.
    Cron {
        /// Canonical `minute hour day-of-month month day-of-week` expression.
        expression: String,
        /// Whether the record continues after its first dispatch.
        recurring: bool,
        /// Explicit time interpretation; currently local-only by contract.
        timezone: ScheduleTimeZone,
    },
    /// One self-paced loop wakeup. Dispatch waits for an explicit reschedule or stop.
    Wakeup {
        /// Delay chosen for this iteration, bounded to one minute through one hour.
        delay_ms: u64,
    },
}

/// Complete durable active schedule record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRecord {
    id: ScheduleId,
    prompt: String,
    scheduled_at_ms: i64,
    rule: ScheduleRule,
    #[serde(default)]
    created_at_ms: i64,
    #[serde(default)]
    expires_at_ms: Option<i64>,
    #[serde(default)]
    jitter_ms: i64,
}

impl ScheduleRecord {
    /// Construct a one-shot delayed schedule from an already resolved target.
    ///
    /// # Errors
    /// Invalid id/prompt/delay/target is refused.
    pub fn after(
        id: ScheduleId,
        prompt: impl Into<String>,
        delay_ms: u64,
        scheduled_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        Self::build(
            id,
            prompt,
            scheduled_at_ms,
            ScheduleRule::After { delay_ms },
            scheduled_at_ms
                .saturating_sub(i64::try_from(delay_ms).unwrap_or(i64::MAX))
                .max(0),
            None,
            0,
        )
    }

    /// Construct a one-shot absolute schedule.
    ///
    /// # Errors
    /// Invalid id/prompt/target is refused.
    pub fn at(
        id: ScheduleId,
        prompt: impl Into<String>,
        scheduled_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        Self::build(id, prompt, scheduled_at_ms, ScheduleRule::At, 0, None, 0)
    }

    /// Construct a fixed-rate schedule from its current anchor-aligned target.
    ///
    /// # Errors
    /// Invalid id/prompt/interval/target is refused.
    pub fn every(
        id: ScheduleId,
        prompt: impl Into<String>,
        every_ms: u64,
        scheduled_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        Self::build(
            id,
            prompt,
            scheduled_at_ms,
            ScheduleRule::Every { every_ms },
            scheduled_at_ms
                .saturating_sub(i64::try_from(every_ms).unwrap_or(i64::MAX))
                .max(0),
            None,
            0,
        )
    }

    /// Construct a five-field cron schedule with deterministic native-style jitter.
    ///
    /// # Errors
    /// Invalid syntax, a schedule with no bounded future occurrence, timestamp
    /// overflow, or invalid identity/content is refused.
    pub fn cron(
        id: ScheduleId,
        prompt: impl Into<String>,
        expression: impl AsRef<str>,
        recurring: bool,
        created_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        if created_at_ms < 0 {
            return Err(ScheduleProjectionError::Invalid(
                "schedule creation time is invalid",
            ));
        }
        let spec = CronSpec::parse(expression.as_ref())?;
        let (scheduled_at_ms, jitter_ms) = next_cron_target(&id, &spec, created_at_ms, recurring)?;
        let expires_at_ms = if recurring {
            Some(created_at_ms.checked_add(SCHEDULE_EXPIRY_MS).ok_or(
                ScheduleProjectionError::Invalid("schedule expiry overflowed"),
            )?)
        } else {
            None
        };
        Self::build(
            id,
            prompt,
            scheduled_at_ms,
            ScheduleRule::Cron {
                expression: spec.canonical,
                recurring,
                timezone: ScheduleTimeZone::Local,
            },
            created_at_ms,
            expires_at_ms,
            jitter_ms,
        )
    }

    /// Construct the first pending wakeup for a self-paced session loop.
    ///
    /// # Errors
    /// Delay must be from one minute through one hour and arithmetic must fit.
    pub fn wakeup(
        id: ScheduleId,
        prompt: impl Into<String>,
        delay_ms: u64,
        created_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        if !(MIN_WAKEUP_DELAY_MS..=MAX_WAKEUP_DELAY_MS).contains(&delay_ms) || created_at_ms < 0 {
            return Err(ScheduleProjectionError::Invalid(
                "schedule wakeup delay is invalid",
            ));
        }
        let delay = i64::try_from(delay_ms)
            .map_err(|_| ScheduleProjectionError::Invalid("schedule wakeup delay is invalid"))?;
        let scheduled_at_ms =
            created_at_ms
                .checked_add(delay)
                .ok_or(ScheduleProjectionError::Invalid(
                    "schedule wakeup target overflowed",
                ))?;
        let expires_at_ms = created_at_ms.checked_add(SCHEDULE_EXPIRY_MS).ok_or(
            ScheduleProjectionError::Invalid("schedule expiry overflowed"),
        )?;
        Self::build(
            id,
            prompt,
            scheduled_at_ms,
            ScheduleRule::Wakeup { delay_ms },
            created_at_ms,
            Some(expires_at_ms),
            0,
        )
    }

    fn build(
        id: ScheduleId,
        prompt: impl Into<String>,
        scheduled_at_ms: i64,
        rule: ScheduleRule,
        created_at_ms: i64,
        expires_at_ms: Option<i64>,
        jitter_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        let record = Self {
            id,
            prompt: prompt.into(),
            scheduled_at_ms,
            rule,
            created_at_ms,
            expires_at_ms,
            jitter_ms,
        };
        record.validate()?;
        Ok(record)
    }

    /// Stable id.
    #[must_use]
    pub const fn id(&self) -> &ScheduleId {
        &self.id
    }

    /// Reminder content.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Current next occurrence.
    #[must_use]
    pub const fn scheduled_at_ms(&self) -> i64 {
        self.scheduled_at_ms
    }

    /// Rule.
    #[must_use]
    pub const fn rule(&self) -> &ScheduleRule {
        &self.rule
    }

    /// Creation wall-clock retained for expiry and audit.
    #[must_use]
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    /// Seven-day expiry for recurring cron and self-paced loop records.
    #[must_use]
    pub const fn expires_at_ms(&self) -> Option<i64> {
        self.expires_at_ms
    }

    /// Deterministic offset applied to the cron occurrence.
    #[must_use]
    pub const fn jitter_ms(&self) -> i64 {
        self.jitter_ms
    }

    /// Timezone explicitly retained for cron records.
    #[must_use]
    pub const fn timezone(&self) -> Option<ScheduleTimeZone> {
        match self.rule {
            ScheduleRule::Cron { timezone, .. } => Some(timezone),
            _ => None,
        }
    }

    /// Five-field expression when this is a cron record.
    #[must_use]
    pub fn cron_expression(&self) -> Option<&str> {
        match &self.rule {
            ScheduleRule::Cron { expression, .. } => Some(expression),
            _ => None,
        }
    }

    /// Whether this schedule repeats without an explicit wakeup decision.
    #[must_use]
    pub const fn recurring(&self) -> bool {
        matches!(
            self.rule,
            ScheduleRule::Every { .. }
                | ScheduleRule::Cron {
                    recurring: true,
                    ..
                }
        )
    }

    /// Whether this is a self-paced wakeup, which is never restored on resume.
    #[must_use]
    pub const fn is_wakeup(&self) -> bool {
        matches!(self.rule, ScheduleRule::Wakeup { .. })
    }

    /// Fixed-rate interval when recurring.
    #[must_use]
    pub const fn every_ms(&self) -> Option<u64> {
        match self.rule {
            ScheduleRule::Every { every_ms } => Some(every_ms),
            ScheduleRule::After { .. }
            | ScheduleRule::At
            | ScheduleRule::Cron { .. }
            | ScheduleRule::Wakeup { .. } => None,
        }
    }

    /// Whether dispatch retires this record.
    #[must_use]
    pub const fn one_shot(&self) -> bool {
        match self.rule {
            ScheduleRule::Every { .. } => false,
            ScheduleRule::Cron { recurring, .. } => !recurring,
            ScheduleRule::After { .. } | ScheduleRule::At | ScheduleRule::Wakeup { .. } => true,
        }
    }

    /// Revalidate a deserialized record.
    ///
    /// # Errors
    /// Id, prompt, target, or timing-rule bounds are enforced.
    pub fn validate(&self) -> Result<(), ScheduleProjectionError> {
        self.id.validate()?;
        if !valid_text(&self.prompt, MAX_SCHEDULE_PROMPT_BYTES)
            || self.scheduled_at_ms < 0
            || self.created_at_ms < 0
            || self
                .expires_at_ms
                .is_some_and(|expiry| expiry <= self.created_at_ms)
        {
            return Err(ScheduleProjectionError::Invalid(
                "schedule record is invalid",
            ));
        }
        match &self.rule {
            ScheduleRule::After { delay_ms }
                if *delay_ms == 0
                    || *delay_ms > MAX_SCHEDULE_DELAY_MS
                    || self.expires_at_ms.is_some()
                    || self.jitter_ms != 0 =>
            {
                Err(ScheduleProjectionError::Invalid(
                    "schedule delay is invalid",
                ))
            }
            ScheduleRule::At if self.expires_at_ms.is_some() || self.jitter_ms != 0 => Err(
                ScheduleProjectionError::Invalid("absolute schedule metadata is invalid"),
            ),
            ScheduleRule::Every { every_ms }
                if *every_ms < MIN_SCHEDULE_EVERY_MS
                    || self.expires_at_ms.is_some()
                    || self.jitter_ms != 0 =>
            {
                Err(ScheduleProjectionError::Invalid(
                    "schedule recurrence is too frequent",
                ))
            }
            ScheduleRule::Cron {
                expression,
                recurring,
                timezone: ScheduleTimeZone::Local,
            } => {
                let spec = CronSpec::parse(expression)?;
                if spec.canonical != *expression
                    || recurring != &self.expires_at_ms.is_some()
                    || self.expires_at_ms.is_some_and(|expiry| {
                        self.created_at_ms.checked_add(SCHEDULE_EXPIRY_MS) != Some(expiry)
                    })
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "cron schedule metadata is invalid",
                    ));
                }
                let base = self.scheduled_at_ms.checked_sub(self.jitter_ms).ok_or(
                    ScheduleProjectionError::Invalid("cron jitter arithmetic overflowed"),
                )?;
                if base <= self.created_at_ms || !spec.matches_timestamp(base) {
                    return Err(ScheduleProjectionError::Invalid(
                        "cron schedule target does not match its expression",
                    ));
                }
                let max_jitter = cron_jitter_bound(&spec, base, *recurring)?;
                if (*recurring && !(0..=max_jitter).contains(&self.jitter_ms))
                    || (!*recurring && !(-max_jitter..=0).contains(&self.jitter_ms))
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "cron schedule jitter is invalid",
                    ));
                }
                Ok(())
            }
            ScheduleRule::Wakeup { delay_ms }
                if !(MIN_WAKEUP_DELAY_MS..=MAX_WAKEUP_DELAY_MS).contains(delay_ms)
                    || self.jitter_ms != 0
                    || self.created_at_ms.checked_add(SCHEDULE_EXPIRY_MS) != self.expires_at_ms =>
            {
                Err(ScheduleProjectionError::Invalid(
                    "schedule wakeup metadata is invalid",
                ))
            }
            _ => Ok(()),
        }
    }
}

/// Versioned schedule mutation with replay support for legacy version-one records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleChange {
    /// Create one never-before-used id.
    Create {
        /// Payload version.
        version: u8,
        /// Complete record.
        schedule: ScheduleRecord,
    },
    /// Delete one active record.
    Delete {
        /// Payload version.
        version: u8,
        /// Active id.
        id: ScheduleId,
    },
    /// Record a correlated inbox enqueue as dispatched.
    Dispatch {
        /// Payload version.
        version: u8,
        /// Active id.
        id: ScheduleId,
        /// Wall-clock decision used for recurrence catch-up.
        accepted_at_ms: i64,
        /// Prior exact inbox insertion.
        message_id: crate::InboxMessageId,
    },
    /// Replace the next pending occurrence of one self-paced loop.
    Reschedule {
        /// Payload version.
        version: u8,
        /// Live self-paced loop id.
        id: ScheduleId,
        /// New bounded delay chosen by the model.
        delay_ms: u64,
        /// Exact next wake time calculated when the decision was accepted.
        scheduled_at_ms: i64,
    },
}

impl ScheduleChange {
    /// Create mutation.
    #[must_use]
    pub const fn create(schedule: ScheduleRecord) -> Self {
        Self::Create {
            version: SCHEDULE_CHANGE_VERSION,
            schedule,
        }
    }

    /// Delete mutation.
    #[must_use]
    pub const fn delete(id: ScheduleId) -> Self {
        Self::Delete {
            version: SCHEDULE_CHANGE_VERSION,
            id,
        }
    }

    /// Dispatch mutation after enqueue succeeded.
    ///
    /// # Errors
    /// Invalid id/time/message identity is refused.
    pub fn dispatch(
        id: ScheduleId,
        accepted_at_ms: i64,
        message_id: crate::InboxMessageId,
    ) -> Result<Self, ScheduleProjectionError> {
        let change = Self::Dispatch {
            version: SCHEDULE_CHANGE_VERSION,
            id,
            accepted_at_ms,
            message_id,
        };
        change.validate_shape()?;
        Ok(change)
    }

    /// Self-paced reschedule mutation.
    ///
    /// # Errors
    /// Invalid identity, delay, or target is refused.
    pub fn reschedule(
        id: ScheduleId,
        delay_ms: u64,
        scheduled_at_ms: i64,
    ) -> Result<Self, ScheduleProjectionError> {
        let change = Self::Reschedule {
            version: SCHEDULE_CHANGE_VERSION,
            id,
            delay_ms,
            scheduled_at_ms,
        };
        change.validate_shape()?;
        Ok(change)
    }

    pub(crate) fn validate_shape(&self) -> Result<(), ScheduleProjectionError> {
        let (version, id) = match self {
            Self::Create { version, schedule } => {
                if !matches!(
                    *version,
                    LEGACY_SCHEDULE_CHANGE_VERSION | SCHEDULE_CHANGE_VERSION
                ) {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule change version is unsupported",
                    ));
                }
                if *version == LEGACY_SCHEDULE_CHANGE_VERSION
                    && matches!(
                        schedule.rule(),
                        ScheduleRule::Cron { .. } | ScheduleRule::Wakeup { .. }
                    )
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "new schedule rule requires change version two",
                    ));
                }
                return schedule.validate();
            }
            Self::Delete { version, id }
            | Self::Dispatch { version, id, .. }
            | Self::Reschedule { version, id, .. } => (*version, id),
        };
        if !matches!(
            version,
            LEGACY_SCHEDULE_CHANGE_VERSION | SCHEDULE_CHANGE_VERSION
        ) || matches!(self, Self::Reschedule { .. }) && version != SCHEDULE_CHANGE_VERSION
        {
            return Err(ScheduleProjectionError::Invalid(
                "schedule change version is unsupported",
            ));
        }
        id.validate()?;
        if let Self::Dispatch {
            accepted_at_ms,
            message_id,
            ..
        } = self
            && (*accepted_at_ms < 0 || crate::InboxMessageId::new(message_id.as_str()).is_err())
        {
            return Err(ScheduleProjectionError::Invalid(
                "schedule dispatch is invalid",
            ));
        }
        if let Self::Reschedule {
            delay_ms,
            scheduled_at_ms,
            ..
        } = self
            && (!(MIN_WAKEUP_DELAY_MS..=MAX_WAKEUP_DELAY_MS).contains(delay_ms)
                || *scheduled_at_ms < 0)
        {
            return Err(ScheduleProjectionError::Invalid(
                "schedule wakeup reschedule is invalid",
            ));
        }
        Ok(())
    }
}

/// Strict local-suffix schedule replay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleProjection {
    active: BTreeMap<ScheduleId, ScheduleRecord>,
    armed: BTreeSet<ScheduleId>,
    seen_ids: BTreeSet<ScheduleId>,
    dispatch_counts: BTreeMap<ScheduleId, u64>,
}

impl ScheduleProjection {
    /// Find one active record.
    #[must_use]
    pub fn get(&self, id: &ScheduleId) -> Option<&ScheduleRecord> {
        self.active.get(id)
    }

    /// Active records in stable id order.
    pub fn iter(&self) -> impl Iterator<Item = (&ScheduleId, &ScheduleRecord)> {
        self.active.iter()
    }

    /// Number of live records, including a self-paced loop awaiting a decision.
    #[must_use]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// Whether no live records remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Whether this record currently owns a timer rather than awaiting reschedule.
    #[must_use]
    pub fn is_armed(&self, id: &ScheduleId) -> bool {
        self.armed.contains(id)
    }

    /// Number of durable dispatches for an id.
    #[must_use]
    pub fn dispatch_count(&self, id: &ScheduleId) -> u64 {
        self.dispatch_counts.get(id).copied().unwrap_or(0)
    }
}

/// Strict schedule replay failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleProjectionError {
    /// One bounded shape/transition/correlation invariant failed.
    #[error("invalid durable schedule state: {0}")]
    Invalid(&'static str),
}

/// Fold schedule state from one session's physical-local suffix.
///
/// `local_start_seq` is the fork seed length/first local sequence. Schedule and
/// enqueue events below it are inherited context and deliberately ignored.
///
/// # Errors
/// Malformed changes, reused ids, inactive transitions, invalid recurrence
/// arithmetic, or dispatch without its prior exact enqueue fail loud.
pub fn project_schedules(
    events: &[SessionEvent],
    local_start_seq: u64,
) -> Result<ScheduleProjection, ScheduleProjectionError> {
    let mut projection = ScheduleProjection::default();
    let mut enqueues = BTreeMap::<crate::InboxMessageId, (ScheduleId, i64)>::new();
    let mut dispatched_messages = BTreeSet::new();
    for event in events.iter().filter(|event| event.seq >= local_start_seq) {
        if let SessionEventKind::AgentInboxSplice { inserted, .. } = &event.kind {
            for message in inserted {
                if let InboxSource::Schedule {
                    schedule_id,
                    occurrence_at_ms,
                } = message.source()
                    && enqueues
                        .insert(
                            message.id().clone(),
                            (schedule_id.clone(), *occurrence_at_ms),
                        )
                        .is_some()
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule enqueue identity was reused",
                    ));
                }
            }
        }
        let SessionEventKind::ScheduleChange { change } = &event.kind else {
            continue;
        };
        change.validate_shape()?;
        match change.as_ref() {
            ScheduleChange::Create { version, schedule } => {
                if !projection.seen_ids.insert(schedule.id.clone()) {
                    return Err(ScheduleProjectionError::Invalid("schedule id was reused"));
                }
                if *version == SCHEDULE_CHANGE_VERSION
                    && projection.active.len() >= MAX_ACTIVE_SCHEDULES
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "active schedule limit exceeded",
                    ));
                }
                projection.armed.insert(schedule.id.clone());
                projection
                    .active
                    .insert(schedule.id.clone(), schedule.clone());
            }
            ScheduleChange::Delete { id, .. } => {
                if projection.active.remove(id).is_none() {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule delete targets an inactive id",
                    ));
                }
                projection.armed.remove(id);
            }
            ScheduleChange::Dispatch {
                id,
                accepted_at_ms,
                message_id,
                ..
            } => {
                if !dispatched_messages.insert(message_id.clone()) {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule inbox message was dispatched twice",
                    ));
                }
                let (enqueued_id, occurrence) =
                    enqueues
                        .get(message_id)
                        .ok_or(ScheduleProjectionError::Invalid(
                            "schedule dispatch lacks a prior inbox enqueue",
                        ))?;
                let record =
                    projection
                        .active
                        .get(id)
                        .cloned()
                        .ok_or(ScheduleProjectionError::Invalid(
                            "schedule dispatch targets an inactive id",
                        ))?;
                if enqueued_id != id
                    || *occurrence != record.scheduled_at_ms
                    || *accepted_at_ms < record.scheduled_at_ms
                    || !projection.armed.remove(id)
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule dispatch correlation is stale",
                    ));
                }
                let dispatch_count = projection.dispatch_counts.entry(id.clone()).or_default();
                *dispatch_count = dispatch_count.saturating_add(1);
                match record.rule.clone() {
                    ScheduleRule::After { .. } | ScheduleRule::At => {
                        projection.active.remove(id);
                    }
                    ScheduleRule::Every { every_ms } => {
                        let elapsed =
                            u64::try_from(accepted_at_ms.saturating_sub(record.scheduled_at_ms))
                                .map_err(|_| {
                                    ScheduleProjectionError::Invalid(
                                        "schedule recurrence arithmetic is invalid",
                                    )
                                })?;
                        let intervals = elapsed
                            .checked_div(every_ms)
                            .and_then(|value| value.checked_add(1))
                            .ok_or(ScheduleProjectionError::Invalid(
                                "schedule recurrence arithmetic overflowed",
                            ))?;
                        let advance = intervals.checked_mul(every_ms).ok_or(
                            ScheduleProjectionError::Invalid(
                                "schedule recurrence arithmetic overflowed",
                            ),
                        )?;
                        let advance = i64::try_from(advance).map_err(|_| {
                            ScheduleProjectionError::Invalid(
                                "schedule recurrence target overflowed",
                            )
                        })?;
                        let next = record.scheduled_at_ms.checked_add(advance).ok_or(
                            ScheduleProjectionError::Invalid(
                                "schedule recurrence target overflowed",
                            ),
                        )?;
                        let mut next_record = record;
                        next_record.scheduled_at_ms = next;
                        projection.active.insert(id.clone(), next_record);
                        projection.armed.insert(id.clone());
                    }
                    ScheduleRule::Cron {
                        expression,
                        recurring,
                        ..
                    } => {
                        if !recurring {
                            projection.active.remove(id);
                            continue;
                        }
                        if record
                            .expires_at_ms
                            .is_some_and(|expiry| *accepted_at_ms >= expiry)
                        {
                            projection.active.remove(id);
                            continue;
                        }
                        let spec = CronSpec::parse(&expression)?;
                        let (next, jitter) = next_cron_target(id, &spec, *accepted_at_ms, true)?;
                        let mut next_record = record;
                        next_record.scheduled_at_ms = next;
                        next_record.jitter_ms = jitter;
                        projection.active.insert(id.clone(), next_record);
                        projection.armed.insert(id.clone());
                    }
                    ScheduleRule::Wakeup { .. } => {
                        if record
                            .expires_at_ms
                            .is_some_and(|expiry| *accepted_at_ms >= expiry)
                        {
                            projection.active.remove(id);
                        }
                    }
                }
            }
            ScheduleChange::Reschedule {
                id,
                delay_ms,
                scheduled_at_ms,
                ..
            } => {
                let record =
                    projection
                        .active
                        .get_mut(id)
                        .ok_or(ScheduleProjectionError::Invalid(
                            "schedule reschedule targets an inactive id",
                        ))?;
                if !record.is_wakeup()
                    || *scheduled_at_ms <= event.time_ms
                    || record
                        .expires_at_ms
                        .is_some_and(|expiry| event.time_ms >= expiry)
                {
                    return Err(ScheduleProjectionError::Invalid(
                        "schedule reschedule targets an ineligible wakeup",
                    ));
                }
                record.scheduled_at_ms = *scheduled_at_ms;
                record.rule = ScheduleRule::Wakeup {
                    delay_ms: *delay_ms,
                };
                projection.armed.insert(id.clone());
            }
        }
    }
    Ok(projection)
}

#[derive(Debug, Clone)]
struct CronSpec {
    canonical: String,
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

#[derive(Debug, Clone)]
struct CronField {
    values: BTreeSet<u32>,
    unrestricted: bool,
}

impl CronSpec {
    fn parse(expression: &str) -> Result<Self, ScheduleProjectionError> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(ScheduleProjectionError::Invalid(
                "cron expression must have five fields",
            ));
        }
        Ok(Self {
            canonical: fields.join(" "),
            minute: CronField::parse(fields[0], 0, 59, false)?,
            hour: CronField::parse(fields[1], 0, 23, false)?,
            day_of_month: CronField::parse(fields[2], 1, 31, false)?,
            month: CronField::parse(fields[3], 1, 12, false)?,
            day_of_week: CronField::parse(fields[4], 0, 7, true)?,
        })
    }

    fn matches_date(&self, date: NaiveDate) -> bool {
        if !self.month.values.contains(&date.month()) {
            return false;
        }
        let day_of_month = self.day_of_month.values.contains(&date.day());
        let day_of_week = self
            .day_of_week
            .values
            .contains(&date.weekday().num_days_from_sunday());
        match (
            self.day_of_month.unrestricted,
            self.day_of_week.unrestricted,
        ) {
            (true, true) => true,
            (true, false) => day_of_week,
            (false, true) => day_of_month,
            (false, false) => day_of_month || day_of_week,
        }
    }

    fn matches_timestamp(&self, timestamp_ms: i64) -> bool {
        let LocalResult::Single(local) = Local.timestamp_millis_opt(timestamp_ms) else {
            return false;
        };
        timestamp_ms.rem_euclid(60_000) == 0
            && self.matches_date(local.date_naive())
            && self.minute.values.contains(&local.minute())
            && self.hour.values.contains(&local.hour())
    }
}

impl CronField {
    fn parse(
        field: &str,
        minimum: u32,
        maximum: u32,
        sunday_seven: bool,
    ) -> Result<Self, ScheduleProjectionError> {
        if field.is_empty() {
            return Err(ScheduleProjectionError::Invalid("cron field is empty"));
        }
        let mut values = BTreeSet::new();
        let mut unrestricted = false;
        for item in field.split(',') {
            if item.is_empty() {
                return Err(ScheduleProjectionError::Invalid("cron list is invalid"));
            }
            let mut step_parts = item.split('/');
            let base = step_parts.next().unwrap_or_default();
            let step = step_parts
                .next()
                .map(str::parse::<u32>)
                .transpose()
                .map_err(|_| ScheduleProjectionError::Invalid("cron step is invalid"))?
                .unwrap_or(1);
            if step == 0 || step_parts.next().is_some() {
                return Err(ScheduleProjectionError::Invalid("cron step is invalid"));
            }
            unrestricted |= base == "*" && step == 1;
            let (start, end) = if base == "*" {
                (minimum, maximum)
            } else if let Some((start, end)) = base.split_once('-') {
                (
                    parse_cron_value(start, minimum, maximum)?,
                    parse_cron_value(end, minimum, maximum)?,
                )
            } else {
                let value = parse_cron_value(base, minimum, maximum)?;
                (value, if item.contains('/') { maximum } else { value })
            };
            if start > end {
                return Err(ScheduleProjectionError::Invalid("cron range is invalid"));
            }
            let mut value = start;
            loop {
                values.insert(if sunday_seven && value == 7 { 0 } else { value });
                let Some(next) = value.checked_add(step) else {
                    break;
                };
                if next > end {
                    break;
                }
                value = next;
            }
        }
        Ok(Self {
            unrestricted,
            values,
        })
    }
}

fn parse_cron_value(
    value: &str,
    minimum: u32,
    maximum: u32,
) -> Result<u32, ScheduleProjectionError> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(ScheduleProjectionError::Invalid("cron value is invalid"))
}

fn next_cron_target(
    id: &ScheduleId,
    spec: &CronSpec,
    after_ms: i64,
    recurring: bool,
) -> Result<(i64, i64), ScheduleProjectionError> {
    let mut cursor = after_ms;
    loop {
        let base = next_cron_base(spec, cursor)?;
        let bound = cron_jitter_bound(spec, base, recurring)?;
        let magnitude = deterministic_jitter(id, bound)?;
        let jitter = if recurring { magnitude } else { -magnitude };
        let target = base
            .checked_add(jitter)
            .ok_or(ScheduleProjectionError::Invalid(
                "cron jitter arithmetic overflowed",
            ))?;
        if target > after_ms {
            return Ok((target, jitter));
        }
        cursor = base;
    }
}

fn next_cron_base(spec: &CronSpec, after_ms: i64) -> Result<i64, ScheduleProjectionError> {
    let local = match Local.timestamp_millis_opt(after_ms) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(first, second) => first.min(second),
        LocalResult::None => {
            return Err(ScheduleProjectionError::Invalid(
                "cron reference timestamp is invalid",
            ));
        }
    };
    let mut date = local.date_naive();
    for _ in 0..CRON_SEARCH_DAYS {
        if spec.matches_date(date) {
            for hour in &spec.hour.values {
                for minute in &spec.minute.values {
                    let Some(naive) = date.and_hms_opt(*hour, *minute, 0) else {
                        continue;
                    };
                    let candidate = match Local.from_local_datetime(&naive) {
                        LocalResult::Single(value) => Some(value),
                        LocalResult::Ambiguous(first, second) => Some(first.min(second)),
                        LocalResult::None => None,
                    };
                    if let Some(candidate) = candidate {
                        let timestamp = candidate.timestamp_millis();
                        if timestamp > after_ms {
                            return Ok(timestamp);
                        }
                    }
                }
            }
        }
        date = date.succ_opt().ok_or(ScheduleProjectionError::Invalid(
            "cron calendar arithmetic overflowed",
        ))?;
    }
    Err(ScheduleProjectionError::Invalid(
        "cron expression has no bounded future occurrence",
    ))
}

fn cron_jitter_bound(
    spec: &CronSpec,
    base_ms: i64,
    recurring: bool,
) -> Result<i64, ScheduleProjectionError> {
    if !recurring {
        let local = match Local.timestamp_millis_opt(base_ms) {
            LocalResult::Single(value) => value,
            LocalResult::Ambiguous(first, second) => first.min(second),
            LocalResult::None => {
                return Err(ScheduleProjectionError::Invalid(
                    "cron target timezone is invalid",
                ));
            }
        };
        return Ok(if matches!(local.minute(), 0 | 30) {
            ONE_SHOT_JITTER_MAX_MS
        } else {
            0
        });
    }
    let next = next_cron_base(spec, base_ms)?;
    let interval = next
        .checked_sub(base_ms)
        .ok_or(ScheduleProjectionError::Invalid(
            "cron interval arithmetic overflowed",
        ))?;
    Ok(if interval < 60 * 60 * 1_000 {
        interval / 2
    } else {
        RECURRING_JITTER_MAX_MS
    })
}

fn deterministic_jitter(id: &ScheduleId, maximum_ms: i64) -> Result<i64, ScheduleProjectionError> {
    if maximum_ms <= 0 {
        return Ok(0);
    }
    let digest = Sha256::digest(id.as_str().as_bytes());
    let bytes: [u8; 8] = digest[..8]
        .try_into()
        .map_err(|_| ScheduleProjectionError::Invalid("cron jitter hash is invalid"))?;
    let maximum = u64::try_from(maximum_ms)
        .map_err(|_| ScheduleProjectionError::Invalid("cron jitter bound is invalid"))?;
    let value = u64::from_be_bytes(bytes) % maximum.saturating_add(1);
    i64::try_from(value)
        .map_err(|_| ScheduleProjectionError::Invalid("cron jitter value is invalid"))
}
