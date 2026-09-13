//! Durable O07 team roster, task-DAG and peer-mailbox projection.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{SessionEvent, SessionEventKind};

const MAX_ID_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const MAX_TASKS: usize = 1_024;
const MAX_MEMBERS: usize = 32;
const MAX_DEPENDENCIES: usize = 64;
const MAX_MAILBOX_MESSAGES: usize = 4_096;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Invalid team-domain metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TeamMetadataError {
    /// An opaque id was empty, oversized or unsafe.
    #[error("team identifier is invalid")]
    InvalidId,
    /// Human-facing text was empty, oversized or control-bearing.
    #[error("team text is invalid")]
    InvalidText,
    /// A task dependency set was empty-invalid, excessive or duplicated.
    #[error("team task dependencies are invalid")]
    InvalidDependencies,
    /// A change payload was internally incoherent.
    #[error("team change is invalid")]
    InvalidChange,
}

macro_rules! opaque_team_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validate one opaque team-domain identity.
            ///
            /// # Errors
            /// Empty, oversized, surrounding-whitespace or control-bearing values fail.
            pub fn new(value: impl Into<String>) -> Result<Self, TeamMetadataError> {
                let value = value.into();
                if !valid_id(&value) {
                    return Err(TeamMetadataError::InvalidId);
                }
                Ok(Self(value))
            }

            /// Exact opaque text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_team_id!(TeamId, "Identity of one durable team.");
opaque_team_id!(TeamMemberId, "Identity of one authority-bound team member.");
opaque_team_id!(TeamTaskId, "Identity of one task in a team DAG.");
opaque_team_id!(
    TeamMessageId,
    "Identity of one insert-once peer-mailbox message."
);

/// Authority role assigned to one team member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    /// Owns roster, dependency and recovery mutations.
    Lead,
    /// May complete assigned work and exchange peer mail.
    Worker,
    /// Worker whose declared responsibility is review.
    Reviewer,
}

/// One durable roster row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    id: TeamMemberId,
    display: String,
    role: TeamRole,
}

impl TeamMember {
    /// Validate one roster row.
    ///
    /// # Errors
    /// Empty, oversized or control-bearing display text fails.
    pub fn new(
        id: TeamMemberId,
        display: impl Into<String>,
        role: TeamRole,
    ) -> Result<Self, TeamMetadataError> {
        let display = display.into();
        if !valid_one_line(&display, MAX_LABEL_BYTES) {
            return Err(TeamMetadataError::InvalidText);
        }
        Ok(Self { id, display, role })
    }

    /// Stable member id.
    #[must_use]
    pub const fn id(&self) -> &TeamMemberId {
        &self.id
    }

    /// Human-facing name.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Authority role.
    #[must_use]
    pub const fn role(&self) -> TeamRole {
        self.role
    }
}

/// Durable task lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamTaskState {
    /// Defined but not currently running.
    Pending,
    /// A background child owns an active attempt.
    InProgress,
    /// Recovery or an explicit dependency prevents dispatch.
    Blocked,
    /// The assigned child completed the task.
    Completed,
    /// The assigned child returned a failure.
    Failed,
    /// The lead cancelled the task.
    Cancelled,
    /// Durable work deletion tombstone.
    Deleted,
}

/// One revisioned task snapshot in a team DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamTask {
    id: TeamTaskId,
    title: String,
    assignee: TeamMemberId,
    #[serde(default = "default_assigned")]
    assigned: bool,
    dependencies: Vec<TeamTaskId>,
    revision: u64,
    state: TeamTaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_summary: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    metadata: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    creation_fingerprint: Option<String>,
}

impl TeamTask {
    /// Create a revision-one pending task.
    ///
    /// # Errors
    /// Invalid title, duplicate/self/excessive dependencies, or unsafe ids fail.
    pub fn new(
        id: TeamTaskId,
        title: impl Into<String>,
        assignee: TeamMemberId,
        dependencies: Vec<TeamTaskId>,
    ) -> Result<Self, TeamMetadataError> {
        let title = title.into();
        validate_dependencies(&id, &dependencies)?;
        if !valid_one_line(&title, MAX_LABEL_BYTES)
            || !valid_id(id.as_str())
            || !valid_id(assignee.as_str())
        {
            return Err(TeamMetadataError::InvalidText);
        }
        Ok(Self {
            id,
            title,
            assignee,
            assigned: true,
            dependencies,
            revision: 1,
            state: TeamTaskState::Pending,
            result_summary: None,
            description: String::new(),
            metadata: BTreeMap::new(),
            creation_fingerprint: None,
        })
    }

    /// Prepare a replacement for reassignment/recovery. Active attempts must settle first.
    pub fn revised(
        &self,
        title: String,
        assignee: TeamMemberId,
        dependencies: Vec<TeamTaskId>,
        state: TeamTaskState,
    ) -> Result<Self, TeamMetadataError> {
        let mut revised = Self::new(self.id.clone(), title, assignee, dependencies)?;
        revised.revision = self
            .revision
            .checked_add(1)
            .ok_or(TeamMetadataError::InvalidChange)?;
        revised.state = state;
        if self.title == revised.title
            && self.assignee == revised.assignee
            && self.dependencies == revised.dependencies
            && !matches!(state, TeamTaskState::Pending | TeamTaskState::InProgress)
        {
            revised.result_summary.clone_from(&self.result_summary);
        }
        revised.description.clone_from(&self.description);
        revised.metadata.clone_from(&self.metadata);
        revised
            .creation_fingerprint
            .clone_from(&self.creation_fingerprint);
        Ok(revised)
    }

    /// Attach structured work content while preserving the original create fingerprint.
    pub fn with_work_content(
        mut self,
        description: String,
        metadata: BTreeMap<String, serde_json::Value>,
        creation_fingerprint: Option<String>,
    ) -> Result<Self, TeamMetadataError> {
        let fields = crate::WorkItemFields {
            subject: self.title.clone(),
            description: description.clone(),
            status: crate::WorkStatus::Pending,
            owner: Some(self.assignee.as_str().into()),
            dependencies: Vec::new(),
            metadata: metadata.clone(),
        };
        fields
            .validate()
            .map_err(|_| TeamMetadataError::InvalidText)?;
        if creation_fingerprint
            .as_ref()
            .is_some_and(|value| value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(TeamMetadataError::InvalidChange);
        }
        self.description = description;
        self.metadata = metadata;
        self.creation_fingerprint = creation_fingerprint;
        Ok(self)
    }
    /// Work instructions independent from the latest execution result.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
    /// Caller-owned structured work metadata.
    #[must_use]
    pub fn metadata(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.metadata
    }
    /// Original create fingerprint when created through the unified work service.
    #[must_use]
    pub fn creation_fingerprint(&self) -> Option<&str> {
        self.creation_fingerprint.as_deref()
    }

    /// Stable task id.
    #[must_use]
    pub const fn id(&self) -> &TeamTaskId {
        &self.id
    }

    /// Human-facing task title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether this task has an explicit owner.
    #[must_use]
    pub const fn assigned(&self) -> bool {
        self.assigned
    }
    /// Set explicit assignment; the lead id remains an internal placeholder when unassigned.
    #[must_use]
    pub const fn with_assignment(mut self, assigned: bool) -> Self {
        self.assigned = assigned;
        self
    }

    /// Assigned roster member.
    #[must_use]
    pub const fn assignee(&self) -> &TeamMemberId {
        &self.assignee
    }

    /// Ordered dependency ids.
    #[must_use]
    pub fn dependencies(&self) -> &[TeamTaskId] {
        &self.dependencies
    }

    /// Task-local CAS revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> TeamTaskState {
        self.state
    }

    /// Bounded terminal/recovery summary.
    #[must_use]
    pub fn result_summary(&self) -> Option<&str> {
        self.result_summary.as_deref()
    }

    /// Whether all dependencies are complete and this task may start.
    #[must_use]
    pub fn is_runnable(&self, team: &TeamView) -> bool {
        self.assigned
            && team.lifecycle == TeamLifecycle::Active
            && matches!(self.state, TeamTaskState::Pending | TeamTaskState::Blocked)
            && self.dependencies.iter().all(|dependency| {
                team.tasks
                    .get(dependency)
                    .is_some_and(|task| task.state == TeamTaskState::Completed)
            })
    }
}

/// One durable peer-mailbox message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMailboxMessage {
    id: TeamMessageId,
    from: TeamMemberId,
    to: TeamMemberId,
    body: String,
}

impl TeamMailboxMessage {
    /// Validate one insert-once peer message.
    ///
    /// # Errors
    /// Invalid id, same sender/recipient, blank, NUL-bearing or oversized body fails.
    pub fn new(
        id: impl Into<String>,
        from: TeamMemberId,
        to: TeamMemberId,
        body: impl Into<String>,
    ) -> Result<Self, TeamMetadataError> {
        let body = body.into();
        if from == to
            || body.trim().is_empty()
            || body.len() > MAX_MESSAGE_BYTES
            || body.contains('\0')
        {
            return Err(TeamMetadataError::InvalidText);
        }
        Ok(Self {
            id: TeamMessageId::new(id)?,
            from,
            to,
            body,
        })
    }

    /// Stable message id.
    #[must_use]
    pub const fn id(&self) -> &TeamMessageId {
        &self.id
    }

    /// Sender identity.
    #[must_use]
    pub const fn from(&self) -> &TeamMemberId {
        &self.from
    }

    /// Recipient identity.
    #[must_use]
    pub const fn to(&self) -> &TeamMemberId {
        &self.to
    }

    /// Exact durable message body.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// One strict version-one mutation of durable team state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TeamChange {
    /// Admit a complete set of ready native roles as one roster transaction.
    MembersBootstrapped {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting lead.
        actor: TeamMemberId,
        /// Complete set of newly ready roles.
        members: Vec<TeamMember>,
    },
    /// Create revision zero with one lead.
    Created {
        /// Payload schema version.
        version: u8,
        /// New team id.
        team_id: TeamId,
        /// Initial lead row.
        lead: TeamMember,
    },
    /// Add one roster member.
    MemberAdded {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting member.
        actor: TeamMemberId,
        /// Added row.
        member: TeamMember,
    },
    /// Remove a non-lead member after its outstanding work is reassigned.
    MemberRemoved {
        /// Payload version.
        version: u8,
        /// Team identity.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting lead.
        actor: TeamMemberId,
        /// Member being removed.
        member_id: TeamMemberId,
    },
    /// Change admission lifecycle without deleting history.
    LifecycleChanged {
        /// Payload version.
        version: u8,
        /// Team identity.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting lead.
        actor: TeamMemberId,
        /// New lifecycle state.
        lifecycle: TeamLifecycle,
    },
    /// Replace a settled/pending task definition using item-local CAS.
    TaskUpdated {
        /// Payload version.
        version: u8,
        /// Team identity.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting lead.
        actor: TeamMemberId,
        /// Exact prior task revision.
        expected_task_revision: u64,
        /// Full next task definition.
        task: TeamTask,
    },
    /// Add one task definition.
    TaskCreated {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting member.
        actor: TeamMemberId,
        /// Revision-one task.
        task: TeamTask,
    },
    /// Move one task through its lifecycle.
    TaskStateChanged {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting member.
        actor: TeamMemberId,
        /// Target task.
        task_id: TeamTaskId,
        /// Exact prior task revision.
        expected_task_revision: u64,
        /// New task state.
        state: TeamTaskState,
        /// Optional terminal/recovery summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_summary: Option<String>,
    },
    /// Replace one pending task's dependency list.
    TaskDependenciesChanged {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting member.
        actor: TeamMemberId,
        /// Target task.
        task_id: TeamTaskId,
        /// Exact prior task revision.
        expected_task_revision: u64,
        /// Complete replacement dependency list.
        dependencies: Vec<TeamTaskId>,
    },
    /// Insert one peer-mailbox message.
    MessageSent {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting sender.
        actor: TeamMemberId,
        /// Complete message.
        message: TeamMailboxMessage,
    },
    /// Record admission to the recipient's durable conversation inbox.
    MessageDelivered {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Host delivery owner (team lead).
        actor: TeamMemberId,
        /// Delivered message.
        message_id: TeamMessageId,
    },
    /// Mark one message claimed by its recipient.
    MessageClaimed {
        /// Payload schema version.
        version: u8,
        /// Target team.
        team_id: TeamId,
        /// New global revision.
        revision: u64,
        /// Acting recipient.
        actor: TeamMemberId,
        /// Claimed message.
        message_id: TeamMessageId,
    },
}

impl TeamChange {
    /// Create a new team.
    ///
    /// # Errors
    /// The initial member must be a lead.
    pub fn created(team_id: TeamId, lead: TeamMember) -> Result<Self, TeamMetadataError> {
        if lead.role != TeamRole::Lead {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::Created {
            version: 1,
            team_id,
            lead,
        })
    }

    /// Add one roster member at the exact next revision.
    ///
    /// # Errors
    /// Lead-role additions and malformed rows are refused.
    pub fn member_added(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        member: TeamMember,
    ) -> Result<Self, TeamMetadataError> {
        if member.role == TeamRole::Lead || revision == 0 {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::MemberAdded {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            member,
        })
    }

    /// Add one revision-one task.
    ///
    /// # Errors
    /// Zero global revision or non-initial task state fails.
    pub fn task_created(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        task: TeamTask,
    ) -> Result<Self, TeamMetadataError> {
        if revision == 0 || task.revision != 1 || task.state != TeamTaskState::Pending {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::TaskCreated {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            task,
        })
    }

    /// Change one task state using task-local CAS.
    ///
    /// # Errors
    /// Zero revisions or an invalid summary fail.
    #[allow(clippy::too_many_arguments)]
    pub fn task_state_changed(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        task_id: &TeamTaskId,
        expected_task_revision: u64,
        state: TeamTaskState,
        result_summary: Option<String>,
    ) -> Result<Self, TeamMetadataError> {
        if revision == 0
            || expected_task_revision == 0
            || result_summary
                .as_deref()
                .is_some_and(|summary| !valid_body(summary, MAX_MESSAGE_BYTES))
        {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::TaskStateChanged {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            task_id: task_id.clone(),
            expected_task_revision,
            state,
            result_summary,
        })
    }

    /// Replace one task dependency set using task-local CAS.
    ///
    /// # Errors
    /// Zero revisions or an invalid dependency list fails.
    pub fn task_dependencies_changed(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        task_id: &TeamTaskId,
        expected_task_revision: u64,
        dependencies: Vec<TeamTaskId>,
    ) -> Result<Self, TeamMetadataError> {
        if revision == 0 || expected_task_revision == 0 {
            return Err(TeamMetadataError::InvalidChange);
        }
        validate_dependencies(task_id, &dependencies)?;
        Ok(Self::TaskDependenciesChanged {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            task_id: task_id.clone(),
            expected_task_revision,
            dependencies,
        })
    }

    /// Insert one peer-mailbox message.
    ///
    /// # Errors
    /// Zero revision or actor/sender mismatch fails.
    pub fn message_sent(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        message: TeamMailboxMessage,
    ) -> Result<Self, TeamMetadataError> {
        if revision == 0 || actor != message.from() {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::MessageSent {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            message,
        })
    }

    /// Claim one message as its recipient.
    ///
    /// # Errors
    /// Zero revision or invalid message id fails.
    pub fn message_claimed(
        team_id: &TeamId,
        revision: u64,
        actor: &TeamMemberId,
        message_id: impl Into<String>,
    ) -> Result<Self, TeamMetadataError> {
        if revision == 0 {
            return Err(TeamMetadataError::InvalidChange);
        }
        Ok(Self::MessageClaimed {
            version: 1,
            team_id: team_id.clone(),
            revision,
            actor: actor.clone(),
            message_id: TeamMessageId::new(message_id)?,
        })
    }

    pub(crate) fn validate_shape(&self) -> Result<(), TeamMetadataError> {
        if !valid_id(self.team_id().as_str()) {
            return Err(TeamMetadataError::InvalidId);
        }
        match self {
            Self::MemberRemoved {
                version,
                revision,
                actor,
                member_id,
                ..
            } if *version == 1
                && *revision > 0
                && valid_id(actor.as_str())
                && valid_id(member_id.as_str()) =>
            {
                Ok(())
            }
            Self::LifecycleChanged {
                version,
                revision,
                actor,
                ..
            } if *version == 1 && *revision > 0 && valid_id(actor.as_str()) => Ok(()),
            Self::TaskUpdated {
                version,
                revision,
                expected_task_revision,
                task,
                actor,
                ..
            } if *version == 1
                && *revision > 0
                && *expected_task_revision > 0
                && task.revision
                    == expected_task_revision
                        .checked_add(1)
                        .ok_or(TeamMetadataError::InvalidChange)?
                && valid_id(actor.as_str()) =>
            {
                TeamTask::new(
                    task.id.clone(),
                    task.title.clone(),
                    task.assignee.clone(),
                    task.dependencies.clone(),
                )?
                .with_work_content(
                    task.description.clone(),
                    task.metadata.clone(),
                    task.creation_fingerprint.clone(),
                )?;
                Ok(())
            }

            Self::MembersBootstrapped {
                version,
                revision,
                members,
                ..
            } if *version == 1 && *revision > 0 && !members.is_empty() && members.len() <= 8 => {
                for member in members {
                    TeamMember::new(member.id.clone(), member.display.clone(), member.role)?;
                    if member.role == TeamRole::Lead {
                        return Err(TeamMetadataError::InvalidChange);
                    }
                }
                Ok(())
            }
            Self::Created { version, lead, .. } if *version == 1 && lead.role == TeamRole::Lead => {
                Ok(())
            }
            Self::MemberAdded {
                version,
                revision,
                member,
                ..
            } if *version == 1 && *revision > 0 && member.role != TeamRole::Lead => Ok(()),
            Self::TaskCreated {
                version,
                revision,
                task,
                ..
            } if *version == 1
                && *revision > 0
                && task.revision == 1
                && task.state == TeamTaskState::Pending =>
            {
                TeamTask::new(
                    task.id.clone(),
                    task.title.clone(),
                    task.assignee.clone(),
                    task.dependencies.clone(),
                )?
                .with_work_content(
                    task.description.clone(),
                    task.metadata.clone(),
                    task.creation_fingerprint.clone(),
                )?;
                validate_dependencies(&task.id, &task.dependencies)
            }
            Self::TaskStateChanged {
                version,
                revision,
                expected_task_revision,
                result_summary,
                ..
            } if *version == 1
                && *revision > 0
                && *expected_task_revision > 0
                && result_summary
                    .as_deref()
                    .is_none_or(|summary| valid_body(summary, MAX_MESSAGE_BYTES)) =>
            {
                Ok(())
            }
            Self::TaskDependenciesChanged {
                version,
                revision,
                task_id,
                expected_task_revision,
                dependencies,
                ..
            } if *version == 1 && *revision > 0 && *expected_task_revision > 0 => {
                validate_dependencies(task_id, dependencies)
            }
            Self::MessageSent {
                version,
                revision,
                actor,
                message,
                ..
            } if *version == 1 && *revision > 0 && actor == message.from() => Ok(()),
            Self::MessageDelivered {
                version, revision, ..
            }
            | Self::MessageClaimed {
                version, revision, ..
            } if *version == 1 && *revision > 0 => Ok(()),
            _ => Err(TeamMetadataError::InvalidChange),
        }
    }

    fn team_id(&self) -> &TeamId {
        match self {
            Self::Created { team_id, .. }
            | Self::MembersBootstrapped { team_id, .. }
            | Self::MemberAdded { team_id, .. }
            | Self::MemberRemoved { team_id, .. }
            | Self::LifecycleChanged { team_id, .. }
            | Self::TaskUpdated { team_id, .. }
            | Self::TaskCreated { team_id, .. }
            | Self::TaskStateChanged { team_id, .. }
            | Self::TaskDependenciesChanged { team_id, .. }
            | Self::MessageSent { team_id, .. }
            | Self::MessageDelivered { team_id, .. }
            | Self::MessageClaimed { team_id, .. } => team_id,
        }
    }
}

/// Admission lifecycle; archived teams retain all work and mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamLifecycle {
    /// New work may be admitted.
    Active,
    /// Admission stopped while owned workers settle.
    ShuttingDown,
    /// Worker shutdown settled; explicit resume is required.
    Stopped,
    /// History retained; explicit resume is required.
    Archived,
}

/// Rebuilt state for one team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamView {
    id: TeamId,
    lifecycle: TeamLifecycle,
    revision: u64,
    lead: TeamMemberId,
    members: BTreeMap<TeamMemberId, TeamMember>,
    tasks: BTreeMap<TeamTaskId, TeamTask>,
    mailbox: BTreeMap<TeamMessageId, (TeamMailboxMessage, bool, bool)>,
}

impl TeamView {
    /// Current admission lifecycle.
    #[must_use]
    pub const fn lifecycle(&self) -> TeamLifecycle {
        self.lifecycle
    }
    /// Stable team id.
    #[must_use]
    pub const fn id(&self) -> &TeamId {
        &self.id
    }

    /// Last committed global revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Lead identity.
    #[must_use]
    pub const fn lead(&self) -> &TeamMemberId {
        &self.lead
    }

    /// Ordered roster.
    #[must_use]
    pub fn members(&self) -> Vec<&TeamMember> {
        self.members.values().collect()
    }

    /// Ordered tasks.
    #[must_use]
    pub fn tasks(&self) -> Vec<&TeamTask> {
        self.tasks.values().collect()
    }

    /// Exact task lookup.
    #[must_use]
    pub fn task(&self, id: &TeamTaskId) -> Option<&TeamTask> {
        self.tasks.get(id)
    }

    /// Exact roster lookup.
    #[must_use]
    pub fn member(&self, id: &TeamMemberId) -> Option<&TeamMember> {
        self.members.get(id)
    }

    /// Every durable mailbox row with delivery and recipient-claim state.
    #[must_use]
    pub fn mail(&self) -> Vec<(&TeamMailboxMessage, bool, bool)> {
        self.mailbox
            .values()
            .map(|(message, delivered, claimed)| (message, *delivered, *claimed))
            .collect()
    }

    /// Ordered unclaimed mail for one recipient.
    #[must_use]
    pub fn pending_mail(&self, recipient: &TeamMemberId) -> Vec<&TeamMailboxMessage> {
        self.mailbox
            .values()
            .filter(|(message, _, claimed)| !claimed && message.to() == recipient)
            .map(|(message, _, _)| message)
            .collect()
    }
}

/// All teams reconstructed from one session log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamProjection {
    teams: BTreeMap<TeamId, TeamView>,
}

impl TeamProjection {
    /// Exact team lookup.
    #[must_use]
    pub fn team(&self, id: &TeamId) -> Option<&TeamView> {
        self.teams.get(id)
    }

    /// Ordered team snapshot.
    #[must_use]
    pub fn teams(&self) -> Vec<&TeamView> {
        self.teams.values().collect()
    }
}

/// Durable team projection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamProjectionError {
    /// A payload failed its local schema.
    #[error("team change at sequence {seq} is invalid")]
    InvalidShape {
        /// Event sequence.
        seq: u64,
    },
    /// Revision, authority, dependency or mailbox state was inconsistent.
    #[error("team change at sequence {seq} conflicts with durable state")]
    Conflict {
        /// Event sequence.
        seq: u64,
    },
}

/// Reconstruct every team from durable session events.
///
/// # Errors
/// Invalid revisions, actors, ownership, task transitions, DAGs or mailbox
/// claims fail instead of being skipped.
pub fn project_teams(events: &[SessionEvent]) -> Result<TeamProjection, TeamProjectionError> {
    let mut projection = TeamProjection::default();
    for event in events {
        let SessionEventKind::TeamChange { change } = &event.kind else {
            continue;
        };
        change
            .validate_shape()
            .map_err(|_| TeamProjectionError::InvalidShape { seq: event.seq })?;
        apply_change(&mut projection, change, event.seq)?;
    }
    Ok(projection)
}

fn apply_change(
    projection: &mut TeamProjection,
    change: &TeamChange,
    seq: u64,
) -> Result<(), TeamProjectionError> {
    if let TeamChange::Created { team_id, lead, .. } = change {
        if projection.teams.contains_key(team_id) || projection.teams.len() >= 64 {
            return Err(TeamProjectionError::Conflict { seq });
        }
        let mut members = BTreeMap::new();
        members.insert(lead.id.clone(), lead.clone());
        projection.teams.insert(
            team_id.clone(),
            TeamView {
                id: team_id.clone(),
                lifecycle: TeamLifecycle::Active,
                revision: 0,
                lead: lead.id.clone(),
                members,
                tasks: BTreeMap::new(),
                mailbox: BTreeMap::new(),
            },
        );
        return Ok(());
    }
    let team = projection
        .teams
        .get_mut(change.team_id())
        .ok_or(TeamProjectionError::Conflict { seq })?;
    let revision = change_revision(change).ok_or(TeamProjectionError::Conflict { seq })?;
    if revision != team.revision.saturating_add(1) {
        return Err(TeamProjectionError::Conflict { seq });
    }
    if team.lifecycle != TeamLifecycle::Active
        && !matches!(
            change,
            TeamChange::LifecycleChanged { .. }
                | TeamChange::TaskStateChanged { .. }
                | TeamChange::MessageDelivered { .. }
                | TeamChange::MessageClaimed { .. }
        )
    {
        return Err(TeamProjectionError::Conflict { seq });
    }
    if team.lifecycle == TeamLifecycle::Archived
        && !matches!(
            change,
            TeamChange::LifecycleChanged {
                lifecycle: TeamLifecycle::Active,
                ..
            }
        )
    {
        return Err(TeamProjectionError::Conflict { seq });
    }
    match change {
        TeamChange::MemberRemoved {
            actor, member_id, ..
        } => {
            require_lead(team, actor, seq)?;
            if member_id == &team.lead
                || !team.members.contains_key(member_id)
                || team.tasks.values().any(|task| {
                    &task.assignee == member_id
                        && matches!(
                            task.state,
                            TeamTaskState::Pending
                                | TeamTaskState::Blocked
                                | TeamTaskState::InProgress
                        )
                })
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.members.remove(member_id);
        }
        TeamChange::LifecycleChanged {
            actor, lifecycle, ..
        } => {
            require_lead(team, actor, seq)?;
            if team.lifecycle == *lifecycle
                || (matches!(lifecycle, TeamLifecycle::Archived | TeamLifecycle::Stopped)
                    && team
                        .tasks
                        .values()
                        .any(|task| task.state == TeamTaskState::InProgress))
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.lifecycle = *lifecycle;
        }
        TeamChange::TaskUpdated {
            actor,
            expected_task_revision,
            task,
            ..
        } => {
            let current = team
                .tasks
                .get(&task.id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            if actor != &team.lead
                && (!current.assigned
                    || actor != current.assignee()
                    || task.title != current.title
                    || task.description != current.description
                    || task.assignee != current.assignee
                    || task.assigned != current.assigned
                    || task.dependencies != current.dependencies
                    || task.metadata != current.metadata)
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            if current.revision != *expected_task_revision
                || current.state == TeamTaskState::Deleted
                || current.creation_fingerprint != task.creation_fingerprint
                || !team.members.contains_key(&task.assignee)
                || task.dependencies.iter().any(|id| {
                    team.tasks
                        .get(id)
                        .is_none_or(|dependency| dependency.state == TeamTaskState::Deleted)
                })
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            if task.state != TeamTaskState::Completed
                && team.tasks.values().any(|dependent| {
                    dependent.dependencies.contains(&task.id)
                        && (matches!(
                            dependent.state,
                            TeamTaskState::InProgress | TeamTaskState::Completed
                        ) || (task.state == TeamTaskState::Deleted
                            && dependent.state != TeamTaskState::Deleted))
                })
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            if matches!(
                task.state,
                TeamTaskState::InProgress | TeamTaskState::Completed
            ) && task.dependencies.iter().any(|id| {
                team.tasks
                    .get(id)
                    .is_none_or(|dependency| dependency.state != TeamTaskState::Completed)
            }) {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.tasks.insert(task.id.clone(), task.clone());
            ensure_acyclic(team, seq)?;
        }
        TeamChange::Created { .. } => return Err(TeamProjectionError::Conflict { seq }),
        TeamChange::MembersBootstrapped { actor, members, .. } => {
            require_lead(team, actor, seq)?;
            let ids = members.iter().map(TeamMember::id).collect::<BTreeSet<_>>();
            if members.len() + team.members.len() > MAX_MEMBERS
                || ids.len() != members.len()
                || ids.iter().any(|id| team.members.contains_key(*id))
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            for member in members {
                team.members.insert(member.id.clone(), member.clone());
            }
        }
        TeamChange::MemberAdded { actor, member, .. } => {
            require_lead(team, actor, seq)?;
            if team.members.len() >= MAX_MEMBERS || team.members.contains_key(member.id()) {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.members.insert(member.id.clone(), member.clone());
        }
        TeamChange::TaskCreated { actor, task, .. } => {
            require_lead(team, actor, seq)?;
            if team.tasks.len() >= MAX_TASKS
                || team.tasks.contains_key(task.id())
                || !team.members.contains_key(task.assignee())
                || task.dependencies().iter().any(|dependency| {
                    team.tasks
                        .get(dependency)
                        .is_none_or(|task| task.state == TeamTaskState::Deleted)
                })
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.tasks.insert(task.id.clone(), task.clone());
            ensure_acyclic(team, seq)?;
        }
        TeamChange::TaskStateChanged {
            actor,
            task_id,
            expected_task_revision,
            state,
            result_summary,
            ..
        } => {
            let task = team
                .tasks
                .get(task_id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            if task.revision != *expected_task_revision
                || (actor != &team.lead && actor != task.assignee())
                || !valid_transition(task.state, *state)
                || (*state == TeamTaskState::InProgress
                    && (!task.is_runnable(team)
                        || team.tasks.values().any(|other| {
                            other.assignee() == task.assignee()
                                && other.state == TeamTaskState::InProgress
                        })))
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            let task = team
                .tasks
                .get_mut(task_id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            task.revision = task.revision.saturating_add(1);
            task.state = *state;
            task.result_summary.clone_from(result_summary);
        }
        TeamChange::TaskDependenciesChanged {
            actor,
            task_id,
            expected_task_revision,
            dependencies,
            ..
        } => {
            require_lead(team, actor, seq)?;
            if dependencies.iter().any(|dependency| {
                team.tasks
                    .get(dependency)
                    .is_none_or(|task| task.state == TeamTaskState::Deleted)
            }) {
                return Err(TeamProjectionError::Conflict { seq });
            }
            let task = team
                .tasks
                .get_mut(task_id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            if task.revision != *expected_task_revision
                || !matches!(task.state, TeamTaskState::Pending | TeamTaskState::Blocked)
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            task.revision = task.revision.saturating_add(1);
            task.dependencies.clone_from(dependencies);
            ensure_acyclic(team, seq)?;
        }
        TeamChange::MessageSent { actor, message, .. } => {
            if actor != message.from()
                || !team.members.contains_key(actor)
                || !team.members.contains_key(message.to())
                || team.mailbox.len() >= MAX_MAILBOX_MESSAGES
                || team.mailbox.contains_key(message.id())
            {
                return Err(TeamProjectionError::Conflict { seq });
            }
            team.mailbox
                .insert(message.id.clone(), (message.clone(), false, false));
        }
        TeamChange::MessageDelivered {
            actor, message_id, ..
        } => {
            require_lead(team, actor, seq)?;
            let (_, delivered, claimed) = team
                .mailbox
                .get_mut(message_id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            if *delivered || *claimed {
                return Err(TeamProjectionError::Conflict { seq });
            }
            *delivered = true;
        }
        TeamChange::MessageClaimed {
            actor, message_id, ..
        } => {
            let (message, _, claimed) = team
                .mailbox
                .get_mut(message_id)
                .ok_or(TeamProjectionError::Conflict { seq })?;
            if *claimed || message.to() != actor {
                return Err(TeamProjectionError::Conflict { seq });
            }
            *claimed = true;
        }
    }
    team.revision = revision;
    Ok(())
}

fn change_revision(change: &TeamChange) -> Option<u64> {
    match change {
        TeamChange::Created { .. } => None,
        TeamChange::MembersBootstrapped { revision, .. }
        | TeamChange::MemberAdded { revision, .. }
        | TeamChange::MemberRemoved { revision, .. }
        | TeamChange::LifecycleChanged { revision, .. }
        | TeamChange::TaskUpdated { revision, .. }
        | TeamChange::TaskCreated { revision, .. }
        | TeamChange::TaskStateChanged { revision, .. }
        | TeamChange::TaskDependenciesChanged { revision, .. }
        | TeamChange::MessageSent { revision, .. }
        | TeamChange::MessageDelivered { revision, .. }
        | TeamChange::MessageClaimed { revision, .. } => Some(*revision),
    }
}

fn require_lead(
    team: &TeamView,
    actor: &TeamMemberId,
    seq: u64,
) -> Result<(), TeamProjectionError> {
    if actor == &team.lead {
        Ok(())
    } else {
        Err(TeamProjectionError::Conflict { seq })
    }
}

fn valid_transition(from: TeamTaskState, to: TeamTaskState) -> bool {
    matches!(
        (from, to),
        (
            TeamTaskState::Pending | TeamTaskState::Blocked,
            TeamTaskState::InProgress
        ) | (
            TeamTaskState::InProgress,
            TeamTaskState::Completed
                | TeamTaskState::Failed
                | TeamTaskState::Blocked
                | TeamTaskState::Cancelled,
        ) | (
            TeamTaskState::Pending | TeamTaskState::Blocked,
            TeamTaskState::Cancelled
        )
    )
}

fn ensure_acyclic(team: &TeamView, seq: u64) -> Result<(), TeamProjectionError> {
    fn visit(
        id: &TeamTaskId,
        tasks: &BTreeMap<TeamTaskId, TeamTask>,
        visiting: &mut BTreeSet<TeamTaskId>,
        visited: &mut BTreeSet<TeamTaskId>,
    ) -> bool {
        if visited.contains(id) {
            return true;
        }
        if !visiting.insert(id.clone()) {
            return false;
        }
        let valid = tasks.get(id).is_some_and(|task| {
            task.dependencies
                .iter()
                .all(|dependency| visit(dependency, tasks, visiting, visited))
        });
        visiting.remove(id);
        if valid {
            visited.insert(id.clone());
        }
        valid
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    if team
        .tasks
        .keys()
        .all(|id| visit(id, &team.tasks, &mut visiting, &mut visited))
    {
        Ok(())
    } else {
        Err(TeamProjectionError::Conflict { seq })
    }
}

fn validate_dependencies(
    task: &TeamTaskId,
    dependencies: &[TeamTaskId],
) -> Result<(), TeamMetadataError> {
    let unique = dependencies.iter().collect::<BTreeSet<_>>();
    if dependencies.len() > MAX_DEPENDENCIES
        || unique.len() != dependencies.len()
        || dependencies.iter().any(|dependency| dependency == task)
    {
        Err(TeamMetadataError::InvalidDependencies)
    } else {
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
}

fn valid_one_line(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_body(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.contains('\0')
}

const fn default_assigned() -> bool {
    true
}
