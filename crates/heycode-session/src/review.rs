//! Durable O14 review requests and structured finding settlements.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::{SessionEvent, SessionEventKind};

const MAX_ID_BYTES: usize = 128;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_INSTRUCTIONS_BYTES: usize = 64 * 1024;
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
const MAX_FINDINGS: usize = 256;
const MAX_FINDING_BODY_BYTES: usize = 16 * 1024;
const MAX_REPORTED_FINDINGS: usize = 128;
const MAX_REPORTED_FIELD_BYTES: usize = 8 * 1024;
const MAX_REPORT_TEXT_BYTES: usize = 256 * 1024;

/// Invalid review-domain metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReviewMetadataError {
    /// Opaque id or runtime/base identity was invalid.
    #[error("review identifier is invalid")]
    InvalidId,
    /// Patch, instructions, summary or finding text was invalid.
    #[error("review text is invalid")]
    InvalidText,
    /// Finding path or line range could escape/misidentify the workspace.
    #[error("review location is invalid")]
    InvalidLocation,
    /// A review settlement was internally incoherent.
    #[error("review change is invalid")]
    InvalidChange,
}

/// Stable review run identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReviewRunId(String);

impl ReviewRunId {
    /// Validate one opaque review id.
    ///
    /// # Errors
    /// Empty, oversized, whitespace or control-bearing values fail.
    pub fn new(value: impl Into<String>) -> Result<Self, ReviewMetadataError> {
        let value = value.into();
        if !valid_id(&value) {
            return Err(ReviewMetadataError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Exact opaque text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ReviewRunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Actionability/severity of one review finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSeverity {
    /// Exploitable/data-loss or release-blocking defect.
    Critical,
    /// Likely correctness/security defect requiring action.
    High,
    /// Bounded defect or important maintainability risk.
    Medium,
    /// Low-impact but concrete improvement.
    Low,
}

/// Effort level used for one direct finding review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLevel {
    /// Fast, narrowly scoped review.
    Low,
    /// Standard review depth.
    Medium,
    /// Thorough review depth.
    High,
    /// Extra-high review depth.
    Xhigh,
    /// Maximum available review depth.
    Max,
}

/// Result of an explicit verification pass for one reported finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FindingVerificationVerdict {
    /// The verification pass reproduced or otherwise established the defect.
    Confirmed,
    /// The defect remains credible but was not conclusively reproduced.
    Plausible,
}

/// Result of an explicitly requested post-fix pass for one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingOutcome {
    /// A fix was applied for the finding.
    Fixed,
    /// The finding was deliberately not changed.
    Skipped,
    /// Verification established that no code change was needed.
    NoChangeNeeded,
}

/// Stable identity for one model-reported finding batch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FindingReportId(String);

impl FindingReportId {
    /// Validate one opaque report id.
    ///
    /// # Errors
    /// Empty, oversized, whitespace or control-bearing values fail.
    pub fn new(value: impl Into<String>) -> Result<Self, ReviewMetadataError> {
        let value = value.into();
        if !valid_id(&value) {
            return Err(ReviewMetadataError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Exact opaque text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FindingReportId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Authority-derived identity of the workspace generation a report describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingReportSource {
    session_id: heycode_core::SessionId,
    workspace_revision: u64,
    root_index: u16,
    cwd: Option<String>,
}

impl FindingReportSource {
    /// Construct one source identity from the owning session and pinned workspace.
    ///
    /// `cwd` is relative to the selected workspace root; `None` means the root
    /// itself. Callers must derive these values from live authority rather than
    /// accepting them from model input.
    ///
    /// # Errors
    /// Invalid session identity, root ordinal, or relative cwd fails.
    pub fn workspace(
        session_id: heycode_core::SessionId,
        workspace_revision: u64,
        root_index: u16,
        cwd: Option<String>,
    ) -> Result<Self, ReviewMetadataError> {
        let source = Self {
            session_id,
            workspace_revision,
            root_index,
            cwd,
        };
        source.validate_shape()?;
        Ok(source)
    }

    /// Durable session that owned the model tool.
    #[must_use]
    pub const fn session_id(&self) -> &heycode_core::SessionId {
        &self.session_id
    }

    /// Revision of the pinned workspace authority generation.
    #[must_use]
    pub const fn workspace_revision(&self) -> u64 {
        self.workspace_revision
    }

    /// Zero-based selected root within that generation.
    #[must_use]
    pub const fn root_index(&self) -> u16 {
        self.root_index
    }

    /// Working directory relative to the selected root; `None` means root.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    fn validate_shape(&self) -> Result<(), ReviewMetadataError> {
        if !valid_id(self.session_id.as_str())
            || self.root_index >= 64
            || self
                .cwd
                .as_deref()
                .is_some_and(|cwd| !valid_relative_path(cwd))
        {
            return Err(ReviewMetadataError::InvalidLocation);
        }
        Ok(())
    }
}

/// One revision-bound finding accepted from the model-facing report tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedFinding {
    id: String,
    severity: ReviewSeverity,
    path: String,
    line_start: u32,
    line_end: u32,
    revision: String,
    title: String,
    trigger: String,
    failure: String,
    impact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<FindingVerificationVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<FindingOutcome>,
}

impl ReportedFinding {
    /// Validate one concrete, revision-bound finding.
    ///
    /// # Errors
    /// Invalid ids, paths, line ranges, revisions, control text, blank text, or
    /// oversized fields fail.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        severity: ReviewSeverity,
        path: impl Into<String>,
        line_start: u32,
        line_end: u32,
        revision: impl Into<String>,
        title: impl Into<String>,
        trigger: impl Into<String>,
        failure: impl Into<String>,
        impact: impl Into<String>,
    ) -> Result<Self, ReviewMetadataError> {
        let finding = Self {
            id: id.into(),
            severity,
            path: path.into(),
            line_start,
            line_end,
            revision: revision.into(),
            title: title.into(),
            trigger: trigger.into(),
            failure: failure.into(),
            impact: impact.into(),
            category: None,
            verdict: None,
            outcome: None,
        };
        finding.validate_shape()?;
        Ok(finding)
    }

    /// Attach optional reference-compatible classification, verification, and
    /// post-fix dimensions.
    ///
    /// # Errors
    /// A category that is not a 1-40 byte lower-case kebab-case slug fails.
    pub fn with_reference_dimensions(
        mut self,
        category: Option<String>,
        verdict: Option<FindingVerificationVerdict>,
        outcome: Option<FindingOutcome>,
    ) -> Result<Self, ReviewMetadataError> {
        self.category = category;
        self.verdict = verdict;
        self.outcome = outcome;
        self.validate_shape()?;
        Ok(self)
    }

    /// Finding-local stable id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Severity.
    #[must_use]
    pub const fn severity(&self) -> ReviewSeverity {
        self.severity
    }

    /// Workspace-root-relative path derived by the service owner.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// First affected one-based line.
    #[must_use]
    pub const fn line_start(&self) -> u32 {
        self.line_start
    }

    /// Last affected one-based line.
    #[must_use]
    pub const fn line_end(&self) -> u32 {
        self.line_end
    }

    /// Exact lowercase SHA-256 revision returned by the file read service.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Short actionable title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Concrete preconditions/actions that reproduce the problem.
    #[must_use]
    pub fn trigger(&self) -> &str {
        &self.trigger
    }

    /// Concrete incorrect behavior or violated invariant.
    #[must_use]
    pub fn failure(&self) -> &str {
        &self.failure
    }

    /// User, security, correctness, or operational consequence.
    #[must_use]
    pub fn impact(&self) -> &str {
        &self.impact
    }

    /// Optional lower-case kebab-case finding classification.
    #[must_use]
    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    /// Optional verdict from an explicit verification pass.
    #[must_use]
    pub const fn verdict(&self) -> Option<FindingVerificationVerdict> {
        self.verdict
    }

    /// Optional result from an explicitly requested post-fix pass.
    #[must_use]
    pub const fn outcome(&self) -> Option<FindingOutcome> {
        self.outcome
    }

    fn validate_shape(&self) -> Result<(), ReviewMetadataError> {
        if !valid_id(&self.id) {
            return Err(ReviewMetadataError::InvalidId);
        }
        if !valid_relative_path(&self.path)
            || self.line_start == 0
            || self.line_end < self.line_start
        {
            return Err(ReviewMetadataError::InvalidLocation);
        }
        if !valid_revision(&self.revision) {
            return Err(ReviewMetadataError::InvalidId);
        }
        if !valid_one_line(&self.title, 256)
            || !valid_untrusted_text(&self.trigger, MAX_REPORTED_FIELD_BYTES)
            || !valid_untrusted_text(&self.failure, MAX_REPORTED_FIELD_BYTES)
            || !valid_untrusted_text(&self.impact, MAX_REPORTED_FIELD_BYTES)
            || self
                .category
                .as_deref()
                .is_some_and(|value| !valid_category(value))
        {
            return Err(ReviewMetadataError::InvalidText);
        }
        Ok(())
    }

    fn same_reported_content(&self, other: &Self) -> bool {
        self.severity == other.severity
            && self.path == other.path
            && self.line_start == other.line_start
            && self.line_end == other.line_end
            && self.revision == other.revision
            && self.title == other.title
            && self.trigger == other.trigger
            && self.failure == other.failure
            && self.impact == other.impact
    }
}

/// One durable batch of findings reported against a pinned workspace generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingReport {
    id: FindingReportId,
    source: FindingReportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<ReviewLevel>,
    findings: Vec<ReportedFinding>,
}

impl FindingReport {
    /// Validate one complete finding report.
    ///
    /// # Errors
    /// Empty/excessive findings, duplicate ids or exact duplicate findings, an
    /// invalid source, or an excessive aggregate text payload fails.
    pub fn new(
        id: FindingReportId,
        source: FindingReportSource,
        findings: Vec<ReportedFinding>,
    ) -> Result<Self, ReviewMetadataError> {
        let report = Self {
            id,
            source,
            level: None,
            findings,
        };
        report.validate_shape()?;
        Ok(report)
    }

    /// Attach the effort level used for this review.
    #[must_use]
    pub fn with_level(mut self, level: ReviewLevel) -> Self {
        self.level = Some(level);
        self
    }

    /// Stable report identity.
    #[must_use]
    pub const fn id(&self) -> &FindingReportId {
        &self.id
    }

    /// Authority-derived workspace source.
    #[must_use]
    pub const fn source(&self) -> &FindingReportSource {
        &self.source
    }

    /// Optional effort level used for this review.
    #[must_use]
    pub const fn level(&self) -> Option<ReviewLevel> {
        self.level
    }

    /// Findings in model-provided order.
    #[must_use]
    pub fn findings(&self) -> &[ReportedFinding] {
        &self.findings
    }

    fn validate_shape(&self) -> Result<(), ReviewMetadataError> {
        self.source.validate_shape()?;
        if self.findings.is_empty() || self.findings.len() > MAX_REPORTED_FINDINGS {
            return Err(ReviewMetadataError::InvalidChange);
        }
        let mut ids = BTreeSet::new();
        let mut text_bytes = 0usize;
        for (index, finding) in self.findings.iter().enumerate() {
            finding.validate_shape()?;
            if !ids.insert(finding.id())
                || self.findings[..index]
                    .iter()
                    .any(|prior| prior.same_reported_content(finding))
            {
                return Err(ReviewMetadataError::InvalidChange);
            }
            text_bytes = text_bytes
                .checked_add(finding.title.len())
                .and_then(|total| total.checked_add(finding.trigger.len()))
                .and_then(|total| total.checked_add(finding.failure.len()))
                .and_then(|total| total.checked_add(finding.impact.len()))
                .and_then(|total| {
                    total.checked_add(finding.category.as_deref().map_or(0, str::len))
                })
                .ok_or(ReviewMetadataError::InvalidChange)?;
        }
        if text_bytes > MAX_REPORT_TEXT_BYTES {
            return Err(ReviewMetadataError::InvalidChange);
        }
        Ok(())
    }
}

/// One validated file-and-line review finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    id: String,
    severity: ReviewSeverity,
    path: String,
    line_start: u32,
    line_end: u32,
    title: String,
    body: String,
}

impl ReviewFinding {
    /// Validate one structured finding.
    ///
    /// # Errors
    /// Invalid id, non-relative/escaping path, zero/reversed line range, or
    /// blank/oversized text fails.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        severity: ReviewSeverity,
        path: impl Into<String>,
        line_start: u32,
        line_end: u32,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, ReviewMetadataError> {
        let id = id.into();
        let path = path.into();
        let title = title.into();
        let body = body.into();
        if !valid_id(&id) {
            return Err(ReviewMetadataError::InvalidId);
        }
        if !valid_relative_path(&path) || line_start == 0 || line_end < line_start {
            return Err(ReviewMetadataError::InvalidLocation);
        }
        if !valid_one_line(&title, 256) || !valid_body(&body, MAX_FINDING_BODY_BYTES) {
            return Err(ReviewMetadataError::InvalidText);
        }
        Ok(Self {
            id,
            severity,
            path,
            line_start,
            line_end,
            title,
            body,
        })
    }

    /// Finding-local stable id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Severity.
    #[must_use]
    pub const fn severity(&self) -> ReviewSeverity {
        self.severity
    }

    /// Portable workspace-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// First affected one-based line.
    #[must_use]
    pub const fn line_start(&self) -> u32 {
        self.line_start
    }

    /// Last affected one-based line.
    #[must_use]
    pub const fn line_end(&self) -> u32 {
        self.line_end
    }

    /// Short actionable title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Evidence and impact explanation.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Closed reason a review produced no accepted structured result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFailureReason {
    /// Caller/lifecycle cancellation.
    Cancelled,
    /// Runtime start, protocol or settlement failure.
    Runtime,
    /// Final output did not match the strict finding schema.
    InvalidOutput,
    /// Reviewer changed its isolated checkout; findings were rejected.
    MutationDetected,
    /// Exact base/patch worktree preparation failed.
    Workspace,
}

/// One durable review start or terminal settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ReviewChange {
    /// Exact model-visible review input committed before runtime dispatch.
    Started {
        /// Payload schema version.
        version: u8,
        /// Review identity.
        run_id: ReviewRunId,
        /// Selected runtime id.
        runtime: String,
        /// Exact lower-case Git commit object id.
        base_commit: String,
        /// Exact bounded Git patch applied to the isolated checkout.
        patch: String,
        /// Exact additional reviewer instructions.
        instructions: String,
    },
    /// Accepted structured findings from a non-mutating isolated run.
    Completed {
        /// Payload schema version.
        version: u8,
        /// Review identity.
        run_id: ReviewRunId,
        /// Durable child session that drove the runtime.
        child_session_id: String,
        /// Bounded review summary.
        summary: String,
        /// Unique structured findings.
        findings: Vec<ReviewFinding>,
    },
    /// Classified terminal failure; no findings are published.
    Failed {
        /// Payload schema version.
        version: u8,
        /// Review identity.
        run_id: ReviewRunId,
        /// Closed failure reason.
        reason: ReviewFailureReason,
    },
    /// One direct model-tool report bound to current file revisions.
    FindingsReported {
        /// Payload schema version.
        version: u8,
        /// Complete bounded report and authority-derived source identity.
        report: Box<FindingReport>,
    },
}

impl ReviewChange {
    /// Build an exact review start.
    ///
    /// # Errors
    /// Invalid runtime/base identity, oversized patch or invalid instructions fail.
    pub fn started(
        run_id: ReviewRunId,
        runtime: impl Into<String>,
        base_commit: impl Into<String>,
        patch: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, ReviewMetadataError> {
        let runtime = runtime.into();
        let base_commit = base_commit.into();
        let patch = patch.into();
        let instructions = instructions.into();
        if !valid_runtime_id(&runtime) || !valid_git_oid(&base_commit) {
            return Err(ReviewMetadataError::InvalidId);
        }
        if patch.len() > MAX_PATCH_BYTES
            || patch.contains('\0')
            || instructions.len() > MAX_INSTRUCTIONS_BYTES
            || instructions.contains('\0')
        {
            return Err(ReviewMetadataError::InvalidText);
        }
        Ok(Self::Started {
            version: 1,
            run_id,
            runtime,
            base_commit,
            patch,
            instructions,
        })
    }

    /// Build one accepted structured settlement.
    ///
    /// # Errors
    /// Invalid child id/summary, excessive findings or duplicate finding ids fail.
    pub fn completed(
        run_id: ReviewRunId,
        child_session_id: impl Into<String>,
        summary: impl Into<String>,
        findings: Vec<ReviewFinding>,
    ) -> Result<Self, ReviewMetadataError> {
        let child_session_id = child_session_id.into();
        let summary = summary.into();
        let unique = findings
            .iter()
            .map(ReviewFinding::id)
            .collect::<BTreeSet<_>>();
        if !valid_id(&child_session_id)
            || !valid_body(&summary, MAX_SUMMARY_BYTES)
            || findings.len() > MAX_FINDINGS
            || unique.len() != findings.len()
        {
            return Err(ReviewMetadataError::InvalidChange);
        }
        Ok(Self::Completed {
            version: 1,
            run_id,
            child_session_id,
            summary,
            findings,
        })
    }

    /// Build a classified terminal failure.
    ///
    /// # Errors
    /// Reserved for forward-compatible constructor symmetry.
    pub fn failed(
        run_id: ReviewRunId,
        reason: ReviewFailureReason,
    ) -> Result<Self, ReviewMetadataError> {
        Ok(Self::Failed {
            version: 1,
            run_id,
            reason,
        })
    }

    /// Build one accepted model-reported finding batch.
    ///
    /// # Errors
    /// Invalid report shape fails before the session append.
    pub fn findings_reported(report: FindingReport) -> Result<Self, ReviewMetadataError> {
        report.validate_shape()?;
        Ok(Self::FindingsReported {
            version: 1,
            report: Box::new(report),
        })
    }

    pub(crate) fn validate_shape(&self) -> Result<(), ReviewMetadataError> {
        match self {
            Self::Started {
                version,
                runtime,
                base_commit,
                patch,
                instructions,
                ..
            } if *version == 1
                && valid_runtime_id(runtime)
                && valid_git_oid(base_commit)
                && patch.len() <= MAX_PATCH_BYTES
                && !patch.contains('\0')
                && instructions.len() <= MAX_INSTRUCTIONS_BYTES
                && !instructions.contains('\0') =>
            {
                Ok(())
            }
            Self::Completed {
                version,
                child_session_id,
                summary,
                findings,
                ..
            } if *version == 1
                && valid_id(child_session_id)
                && valid_body(summary, MAX_SUMMARY_BYTES)
                && findings.len() <= MAX_FINDINGS
                && findings
                    .iter()
                    .map(ReviewFinding::id)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == findings.len() =>
            {
                Ok(())
            }
            Self::Failed { version: 1, .. } => Ok(()),
            Self::FindingsReported { version: 1, report } => report.validate_shape(),
            _ => Err(ReviewMetadataError::InvalidChange),
        }
    }
}

/// Terminal state of one projected review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// Runtime work has no durable settlement yet.
    Running,
    /// Structured findings were accepted.
    Completed,
    /// Review ended without accepted findings.
    Failed(ReviewFailureReason),
}

/// One review reconstructed from durable events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRunView {
    run_id: ReviewRunId,
    runtime: String,
    base_commit: String,
    patch: String,
    instructions: String,
    state: ReviewState,
    child_session_id: Option<String>,
    summary: Option<String>,
    findings: Vec<ReviewFinding>,
}

impl ReviewRunView {
    /// Review identity.
    #[must_use]
    pub const fn run_id(&self) -> &ReviewRunId {
        &self.run_id
    }

    /// Selected runtime.
    #[must_use]
    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    /// Exact base commit.
    #[must_use]
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Exact durable patch.
    #[must_use]
    pub fn patch(&self) -> &str {
        &self.patch
    }

    /// Exact additional instructions.
    #[must_use]
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> ReviewState {
        self.state
    }

    /// Durable delegated child identity after successful settlement.
    #[must_use]
    pub fn child_session_id(&self) -> Option<&str> {
        self.child_session_id.as_deref()
    }

    /// Accepted summary.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Accepted structured findings.
    #[must_use]
    pub fn findings(&self) -> &[ReviewFinding] {
        &self.findings
    }
}

/// All reviews reconstructed from one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewProjection {
    runs: BTreeMap<ReviewRunId, ReviewRunView>,
    reports: Vec<FindingReport>,
}

impl ReviewProjection {
    /// Exact review lookup.
    #[must_use]
    pub fn run(&self, id: &ReviewRunId) -> Option<&ReviewRunView> {
        self.runs.get(id)
    }

    /// Ordered review snapshot.
    #[must_use]
    pub fn runs(&self) -> Vec<&ReviewRunView> {
        self.runs.values().collect()
    }

    /// Exact direct finding-report lookup.
    #[must_use]
    pub fn report(&self, id: &FindingReportId) -> Option<&FindingReport> {
        self.reports.iter().find(|report| report.id() == id)
    }

    /// Direct reports in durable event order.
    #[must_use]
    pub fn reports(&self) -> &[FindingReport] {
        &self.reports
    }
}

/// Review projection failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewProjectionError {
    /// Payload schema was invalid.
    #[error("review change at sequence {seq} is invalid")]
    InvalidShape {
        /// Event sequence.
        seq: u64,
    },
    /// Duplicate start/settlement or settlement without a start.
    #[error("review change at sequence {seq} conflicts with durable state")]
    Conflict {
        /// Event sequence.
        seq: u64,
    },
}

/// Reconstruct review runs from durable session truth.
///
/// # Errors
/// Invalid shapes, duplicate ids, terminal repeats and orphan settlements fail.
pub fn project_reviews(events: &[SessionEvent]) -> Result<ReviewProjection, ReviewProjectionError> {
    let mut projection = ReviewProjection::default();
    for event in events {
        let SessionEventKind::ReviewChange { change } = &event.kind else {
            continue;
        };
        change
            .validate_shape()
            .map_err(|_| ReviewProjectionError::InvalidShape { seq: event.seq })?;
        match change.as_ref() {
            ReviewChange::Started {
                run_id,
                runtime,
                base_commit,
                patch,
                instructions,
                ..
            } => {
                if projection.runs.contains_key(run_id) {
                    return Err(ReviewProjectionError::Conflict { seq: event.seq });
                }
                projection.runs.insert(
                    run_id.clone(),
                    ReviewRunView {
                        run_id: run_id.clone(),
                        runtime: runtime.clone(),
                        base_commit: base_commit.clone(),
                        patch: patch.clone(),
                        instructions: instructions.clone(),
                        state: ReviewState::Running,
                        child_session_id: None,
                        summary: None,
                        findings: Vec::new(),
                    },
                );
            }
            ReviewChange::Completed {
                run_id,
                child_session_id,
                summary,
                findings,
                ..
            } => {
                let run = projection
                    .runs
                    .get_mut(run_id)
                    .ok_or(ReviewProjectionError::Conflict { seq: event.seq })?;
                if run.state != ReviewState::Running {
                    return Err(ReviewProjectionError::Conflict { seq: event.seq });
                }
                run.state = ReviewState::Completed;
                run.child_session_id = Some(child_session_id.clone());
                run.summary = Some(summary.clone());
                run.findings.clone_from(findings);
            }
            ReviewChange::Failed { run_id, reason, .. } => {
                let run = projection
                    .runs
                    .get_mut(run_id)
                    .ok_or(ReviewProjectionError::Conflict { seq: event.seq })?;
                if run.state != ReviewState::Running {
                    return Err(ReviewProjectionError::Conflict { seq: event.seq });
                }
                run.state = ReviewState::Failed(*reason);
            }
            ReviewChange::FindingsReported { report, .. } => {
                if projection
                    .reports
                    .iter()
                    .any(|current| current.id() == report.id())
                {
                    return Err(ReviewProjectionError::Conflict { seq: event.seq });
                }
                projection.reports.push((**report).clone());
            }
        }
    }
    Ok(projection)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
}

fn valid_runtime_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
        })
        && !value.ends_with('-')
        && !value.contains("--")
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

fn valid_untrusted_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_revision(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_category(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 4_096
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
        || Path::new(value).is_absolute()
    {
        return false;
    }
    Path::new(value)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}
