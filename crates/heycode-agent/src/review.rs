//! O14 selectable delegated reviewer with structured, non-mutating settlement.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use heycode_core::{EventBus, ToolSpec};
use heycode_exec::{FileSystemErrorCode, PathRequest, ReadFileSpec, ResolvedPath};
use heycode_llm::CapabilitySupport;
use heycode_runtime::{AgentRuntimeKind, AgentRuntimeRegistry};
use heycode_session::{
    CURRENT_SESSION_LOG_VERSION, FindingOutcome, FindingReport, FindingReportId,
    FindingReportSource, FindingVerificationVerdict, InboxDelivery, ReportedFinding, ReviewChange,
    ReviewFailureReason, ReviewFinding, ReviewLevel, ReviewRunId, ReviewSeverity, Session,
    SessionEvent, SessionEventKind, project_reviews,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::jobs::{JobOutcome, JobRegistry, JobSettlement};
use crate::runtime_subagent::RuntimeSubagentProvider;
use crate::subagent_provider::{
    SubagentContinuation, SubagentProvider as _, SubagentRequest, SubagentSeed,
};
use crate::{
    Agent, DenyAll, GitCommitId, GitWorktreeManager, JobId, WorktreeOutcome, WorktreeRetention,
};

const MAX_REVIEW_NOTICE_BYTES: usize = 7 * 1024;
const REPORT_LOCATION_READ_BYTES: usize = 4;
const MAX_TOOL_REPORTED_FINDINGS: usize = 32;

/// Exact review request committed before the reviewer runtime sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    run_id: ReviewRunId,
    runtime: String,
    base: GitCommitId,
    patch: String,
    instructions: String,
}

impl ReviewRequest {
    /// Validate a selected runtime, exact base, bounded patch and instructions.
    ///
    /// # Errors
    /// Invalid review-domain metadata is refused before any session append.
    pub fn new(
        runtime: impl Into<String>,
        base: GitCommitId,
        patch: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, ReviewError> {
        let run_id =
            ReviewRunId::new(heycode_core::SessionId::generate().to_string()).map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Failed,
                    "review identity could not be created",
                )
            })?;
        let change =
            ReviewChange::started(run_id.clone(), runtime, base.as_str(), patch, instructions)
                .map_err(|_| {
                    ReviewError::new(
                        ReviewErrorCode::Refused,
                        "review request metadata is invalid",
                    )
                })?;
        let ReviewChange::Started {
            runtime,
            patch,
            instructions,
            ..
        } = change
        else {
            return Err(ReviewError::new(
                ReviewErrorCode::Failed,
                "review request construction failed",
            ));
        };
        Ok(Self {
            run_id,
            runtime,
            base,
            patch,
            instructions,
        })
    }

    /// Stable run id.
    #[must_use]
    pub const fn run_id(&self) -> &ReviewRunId {
        &self.run_id
    }

    /// Selected runtime id.
    #[must_use]
    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    /// Exact base commit.
    #[must_use]
    pub const fn base(&self) -> &GitCommitId {
        &self.base
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
}

/// Accepted structured review result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewResult {
    run_id: ReviewRunId,
    child_session_id: String,
    summary: String,
    findings: Vec<ReviewFinding>,
}

impl ReviewResult {
    /// Review run id.
    #[must_use]
    pub const fn run_id(&self) -> &ReviewRunId {
        &self.run_id
    }

    /// Durable delegated child session.
    #[must_use]
    pub fn child_session_id(&self) -> &str {
        &self.child_session_id
    }

    /// Bounded summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Structured findings.
    #[must_use]
    pub fn findings(&self) -> &[ReviewFinding] {
        &self.findings
    }
}

/// Stable reviewer failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewErrorCode {
    /// Runtime/request is not selectable.
    Refused,
    /// Caller or lifecycle cancellation.
    Cancelled,
    /// Exact worktree/base/patch preparation or cleanup failed.
    Workspace,
    /// Delegated runtime failed or violated its event contract.
    Runtime,
    /// Final text did not match the strict schema.
    InvalidOutput,
    /// Reviewer changed the isolated checkout; findings were not accepted.
    MutationDetected,
    /// Durable append/projection or job ownership failed.
    Failed,
}

/// Bounded body-free review failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ReviewError {
    code: ReviewErrorCode,
    message: &'static str,
}

impl ReviewError {
    const fn new(code: ReviewErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }

    /// Stable failure class.
    #[must_use]
    pub const fn code(&self) -> ReviewErrorCode {
        self.code
    }

    const fn durable_reason(&self) -> ReviewFailureReason {
        match self.code {
            ReviewErrorCode::Cancelled => ReviewFailureReason::Cancelled,
            ReviewErrorCode::Workspace => ReviewFailureReason::Workspace,
            ReviewErrorCode::Runtime => ReviewFailureReason::Runtime,
            ReviewErrorCode::InvalidOutput => ReviewFailureReason::InvalidOutput,
            ReviewErrorCode::MutationDetected => ReviewFailureReason::MutationDetected,
            ReviewErrorCode::Refused | ReviewErrorCode::Failed => ReviewFailureReason::Runtime,
        }
    }
}

#[derive(Clone)]
struct ReviewJobHost {
    agent: Weak<Agent>,
    jobs: Arc<JobRegistry>,
}

#[derive(Clone)]
struct FindingReportHost {
    workspace: Weak<crate::workspace_transition::WorkspaceTransitionService>,
    ui: EventBus,
}

/// Effect-owned selectable reviewer service.
pub struct ReviewService {
    session: Arc<Mutex<Session>>,
    runtimes: Arc<AgentRuntimeRegistry>,
    worktrees: Arc<GitWorktreeManager>,
    sessions_root: PathBuf,
    operation: tokio::sync::Mutex<()>,
    job_host: Mutex<Option<ReviewJobHost>>,
    finding_host: Mutex<Option<FindingReportHost>>,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for ReviewService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviewService")
            .field("repository", &"<redacted>")
            .field("shutdown", &self.shutdown.is_cancelled())
            .finish()
    }
}

impl ReviewService {
    /// Bind durable truth, runtime registry and isolated worktree owner.
    #[must_use]
    pub fn new(
        session: Arc<Mutex<Session>>,
        runtimes: Arc<AgentRuntimeRegistry>,
        worktrees: Arc<GitWorktreeManager>,
        sessions_root: PathBuf,
    ) -> Self {
        Self {
            session,
            runtimes,
            worktrees,
            sessions_root,
            operation: tokio::sync::Mutex::new(()),
            job_host: Mutex::new(None),
            finding_host: Mutex::new(None),
            shutdown: CancellationToken::new(),
        }
    }

    /// Attach the existing Agent/JobRegistry durable notice owner.
    ///
    /// # Errors
    /// Duplicate/poisoned attachment fails.
    pub fn attach_job_host(
        &self,
        agent: &Arc<Agent>,
        jobs: Arc<JobRegistry>,
    ) -> Result<(), ReviewError> {
        let mut host = self.job_host.lock().map_err(|_| {
            ReviewError::new(ReviewErrorCode::Failed, "review job host is unavailable")
        })?;
        if host.is_some() {
            return Err(ReviewError::new(
                ReviewErrorCode::Failed,
                "review job host is already attached",
            ));
        }
        *host = Some(ReviewJobHost {
            agent: Arc::downgrade(agent),
            jobs,
        });
        Ok(())
    }

    /// Attach the live workspace authority and user-visible event bus used by
    /// the model-facing finding-report tool.
    ///
    /// # Errors
    /// Duplicate or poisoned attachment fails.
    pub fn attach_finding_host(
        &self,
        workspace: &Arc<crate::workspace_transition::WorkspaceTransitionService>,
        ui: EventBus,
    ) -> Result<(), ReviewError> {
        let mut host = self.finding_host.lock().map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding report host is unavailable",
            )
        })?;
        if host.is_some() {
            return Err(ReviewError::new(
                ReviewErrorCode::Failed,
                "finding report host is already attached",
            ));
        }
        *host = Some(FindingReportHost {
            workspace: Arc::downgrade(workspace),
            ui,
        });
        Ok(())
    }

    /// Cancel active review admission and future background starts.
    pub fn dispose(&self) {
        self.shutdown.cancel();
        if let Ok(mut host) = self.job_host.lock() {
            *host = None;
        }
        if let Ok(mut host) = self.finding_host.lock() {
            *host = None;
        }
    }

    /// Verify and durably commit findings against the exact current file and
    /// workspace revisions before announcing one structured UI event.
    ///
    /// `cwd` is the live [`heycode_tools::ToolCtx`] working directory. Workspace
    /// identity is derived from the attached authority owner; model input can
    /// provide only relative paths and read-tool file revisions.
    ///
    /// # Errors
    /// Invalid/stale source data, cancellation, conflicting durable state, or
    /// unavailable workspace/session owners fail without publishing a report.
    pub async fn report_findings(
        &self,
        findings: Vec<ReportedFinding>,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<FindingReport, ReviewError> {
        self.report_findings_with_level(None, findings, cwd, cancellation)
            .await
    }

    /// Verify and durably commit findings with an optional review-effort level.
    ///
    /// This preserves the same authority derivation, stale-read checks, and
    /// single-commit boundary as [`Self::report_findings`].
    ///
    /// # Errors
    /// Invalid/stale source data, cancellation, conflicting durable state, or
    /// unavailable workspace/session owners fail without publishing a report.
    pub async fn report_findings_with_level(
        &self,
        level: Option<ReviewLevel>,
        findings: Vec<ReportedFinding>,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<FindingReport, ReviewError> {
        if self.shutdown.is_cancelled() || cancellation.is_cancelled() {
            return Err(ReviewError::new(
                ReviewErrorCode::Cancelled,
                "finding report was cancelled",
            ));
        }
        let gate = self.operation.lock();
        let _guard = tokio::select! {
            guard = gate => guard,
            () = cancellation.cancelled() => {
                return Err(ReviewError::new(ReviewErrorCode::Cancelled, "finding report was cancelled"));
            }
            () = self.shutdown.cancelled() => {
                return Err(ReviewError::new(ReviewErrorCode::Cancelled, "review service stopped"));
            }
        };
        let host = self
            .finding_host
            .lock()
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Failed,
                    "finding report host is unavailable",
                )
            })?
            .clone()
            .ok_or_else(|| {
                ReviewError::new(
                    ReviewErrorCode::Failed,
                    "finding report host is unavailable",
                )
            })?;
        let workspace = host.workspace.upgrade().ok_or_else(|| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding workspace owner is unavailable",
            )
        })?;
        let _workspace_lease = workspace.pin_activity().map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding workspace is changing; retry after it settles",
            )
        })?;
        let snapshot = workspace.snapshot().map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding workspace source is unavailable",
            )
        })?;
        let filesystem = workspace.pinned_filesystem().map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding workspace source is unavailable",
            )
        })?;
        let resolved_cwd = filesystem
            .resolve(PathRequest::new(cwd, ".").map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Refused,
                    "finding source working directory is invalid",
                )
            })?)
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Refused,
                    "finding source is outside the active workspace",
                )
            })?;
        let (root_index, root) = snapshot
            .roots
            .iter()
            .enumerate()
            .filter(|(_, root)| resolved_cwd.as_path().starts_with(&root.path))
            .max_by_key(|(_, root)| root.path.components().count())
            .ok_or_else(|| {
                ReviewError::new(
                    ReviewErrorCode::Refused,
                    "finding source is outside the active workspace",
                )
            })?;
        let root_index = u16::try_from(root_index).map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "finding workspace source is invalid",
            )
        })?;
        let relative_cwd = relative_utf8(resolved_cwd.as_path(), &root.path)?;
        let relative_cwd = (!relative_cwd.is_empty()).then_some(relative_cwd);
        let session_id = self
            .session
            .lock()
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::Failed, "review session is unavailable")
            })?
            .id()
            .clone();

        let mut normalized = Vec::with_capacity(findings.len());
        let mut sources = BTreeMap::<PathBuf, (ResolvedPath, String, u32)>::new();
        for finding in findings {
            let request =
                PathRequest::new(resolved_cwd.as_path(), finding.path()).map_err(|_| {
                    ReviewError::new(
                        ReviewErrorCode::Refused,
                        "finding path is invalid; use a workspace-relative read path",
                    )
                })?;
            let resolved = filesystem.resolve(request).map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Refused,
                    "finding path is outside the active workspace",
                )
            })?;
            if !resolved.as_path().starts_with(&root.path) {
                return Err(ReviewError::new(
                    ReviewErrorCode::Refused,
                    "all findings must belong to the active workspace source",
                ));
            }
            let path = relative_utf8(resolved.as_path(), &root.path)?;
            let normalized_finding = ReportedFinding::new(
                finding.id(),
                finding.severity(),
                path,
                finding.line_start(),
                finding.line_end(),
                finding.revision(),
                finding.title(),
                finding.trigger(),
                finding.failure(),
                finding.impact(),
            )
            .and_then(|normalized| {
                normalized.with_reference_dimensions(
                    finding.category().map(str::to_owned),
                    finding.verdict(),
                    finding.outcome(),
                )
            })
            .map_err(|_| invalid_report())?;
            match sources.entry(resolved.as_path().to_path_buf()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((
                        resolved,
                        normalized_finding.revision().to_owned(),
                        normalized_finding.line_end(),
                    ));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let (_, revision, maximum_line) = entry.get_mut();
                    if revision != normalized_finding.revision() {
                        return Err(ReviewError::new(
                            ReviewErrorCode::Refused,
                            "findings for one file must use the same read revision",
                        ));
                    }
                    *maximum_line = (*maximum_line).max(normalized_finding.line_end());
                }
            }
            normalized.push(normalized_finding);
        }

        let report_id = FindingReportId::new(heycode_core::SessionId::generate().to_string())
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Failed,
                    "finding report identity could not be created",
                )
            })?;
        let source =
            FindingReportSource::workspace(session_id, snapshot.revision, root_index, relative_cwd)
                .map_err(|_| {
                    ReviewError::new(
                        ReviewErrorCode::Failed,
                        "finding workspace source is invalid",
                    )
                })?;
        let mut report =
            FindingReport::new(report_id, source, normalized).map_err(|_| invalid_report())?;
        if let Some(level) = level {
            report = report.with_level(level);
        }

        // Run the freshness/location probes immediately before the single
        // append. The workspace lease prevents a generation change across the
        // batch; FileSystemService refuses a stale per-file revision.
        for (_, (path, revision, maximum_line)) in sources {
            let spec = ReadFileSpec::new(path, REPORT_LOCATION_READ_BYTES)
                .and_then(|spec| spec.with_window(maximum_line as usize, 1, None))
                .and_then(|spec| spec.with_expected_revision(revision))
                .map_err(|_| invalid_report())?;
            let output = filesystem
                .read(spec, cancellation.clone())
                .await
                .map_err(report_read_error)?;
            if output.page().is_none_or(|page| page.lines_returned == 0) {
                return Err(ReviewError::new(
                    ReviewErrorCode::Refused,
                    "finding line range is outside the current file",
                ));
            }
        }
        if self.shutdown.is_cancelled() || cancellation.is_cancelled() {
            return Err(ReviewError::new(
                ReviewErrorCode::Cancelled,
                "finding report was cancelled",
            ));
        }
        self.commit(
            ReviewChange::findings_reported(report.clone()).map_err(|_| {
                ReviewError::new(ReviewErrorCode::Failed, "finding report is invalid")
            })?,
        )?;
        host.ui.emit(crate::UiEvent::FindingsReported {
            report: report.clone(),
        });
        Ok(report)
    }

    /// Runtime ids that meet the reviewer contract: delegated loop plus exact
    /// permission callbacks so every mutation request can be denied.
    ///
    /// # Errors
    /// Runtime registry failure.
    pub fn selectable_runtimes(&self) -> Result<Vec<String>, ReviewError> {
        self.runtimes
            .descriptors()
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::Failed, "runtime registry is unavailable")
            })
            .map(|rows| {
                rows.into_iter()
                    .filter(|row| {
                        row.kind() == AgentRuntimeKind::Delegated
                            && row.capabilities().permissions == CapabilitySupport::Supported
                    })
                    .map(|row| row.id().as_str().to_owned())
                    .collect()
            })
    }

    /// Capture current tracked changes and run them as an owned background job.
    /// Completion first commits `review/change`, then enters the Agent inbox,
    /// then publishes the terminal JobRegistry row.
    ///
    /// # Errors
    /// Missing job host, invalid runtime/instructions or background admission failure.
    pub fn start_background(
        self: &Arc<Self>,
        runtime: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<JobId, ReviewError> {
        if self.shutdown.is_cancelled() {
            return Err(ReviewError::new(
                ReviewErrorCode::Cancelled,
                "review service stopped",
            ));
        }
        let runtime = runtime.into();
        if !self.selectable_runtimes()?.iter().any(|id| id == &runtime) {
            return Err(ReviewError::new(
                ReviewErrorCode::Refused,
                "review runtime is not selectable",
            ));
        }
        let instructions = instructions.into();
        let host = self
            .job_host
            .lock()
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::Failed, "review job host is unavailable")
            })?
            .clone()
            .ok_or_else(|| {
                ReviewError::new(ReviewErrorCode::Failed, "review jobs are unavailable")
            })?;
        let service = self.clone();
        let jobs = host.jobs.clone();
        let agent = host.agent;
        host.jobs
            .spawn(
                format!("review with {runtime}"),
                InboxDelivery::FollowUp,
                move |job_id, cancellation| async move {
                    let result = service
                        .review_workspace(runtime, instructions, cancellation.clone())
                        .await;
                    let (outcome, notice) = match result {
                        Ok(result) => (JobOutcome::Completed, render_notice(&result)),
                        Err(error) if error.code() == ReviewErrorCode::Cancelled => {
                            (JobOutcome::Cancelled, "review cancelled".to_owned())
                        }
                        Err(error) => (
                            JobOutcome::Failed,
                            format!("review failed ({:?})", error.code()),
                        ),
                    };
                    let Ok(settlement) = JobSettlement::new(outcome, notice) else {
                        return;
                    };
                    let Some(agent) = agent.upgrade() else {
                        return;
                    };
                    if agent.settle_job(&jobs, &job_id, &settlement).is_err() {
                        agent.ui().emit(crate::UiEvent::Error {
                            message: "review settlement could not be committed".to_owned(),
                        });
                    }
                },
            )
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Failed,
                    "review background admission failed",
                )
            })
    }

    /// Capture exact tracked changes against current HEAD and review them.
    ///
    /// # Errors
    /// Git capture, request validation, runtime or structured settlement failure.
    pub async fn review_workspace(
        &self,
        runtime: impl Into<String>,
        instructions: impl Into<String>,
        cancellation: CancellationToken,
    ) -> Result<ReviewResult, ReviewError> {
        let base = self
            .worktrees
            .resolve_head(cancellation.clone())
            .await
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Workspace,
                    "review base could not be resolved",
                )
            })?;
        let patch = self
            .worktrees
            .tracked_patch(&base, cancellation.clone())
            .await
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Workspace,
                    "review patch could not be captured",
                )
            })?;
        let request = ReviewRequest::new(runtime, base, patch, instructions)?;
        self.review(request, cancellation).await
    }

    /// Run one exact pre-built review request.
    ///
    /// The `review/change started` event commits before worktree creation or
    /// runtime dispatch. Only strict JSON from an unchanged isolated checkout
    /// can commit `completed`; every other terminal path commits `failed`.
    ///
    /// # Errors
    /// Selection, cancellation, workspace, runtime, mutation, output-schema or
    /// durable settlement failure.
    pub async fn review(
        &self,
        request: ReviewRequest,
        cancellation: CancellationToken,
    ) -> Result<ReviewResult, ReviewError> {
        let gate = self.operation.lock();
        let _guard = tokio::select! {
            guard = gate => guard,
            () = cancellation.cancelled() => {
                return Err(ReviewError::new(ReviewErrorCode::Cancelled, "review was cancelled"));
            }
            () = self.shutdown.cancelled() => {
                return Err(ReviewError::new(ReviewErrorCode::Cancelled, "review service stopped"));
            }
        };
        let runtime = self
            .runtimes
            .get(request.runtime())
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::Failed, "runtime registry is unavailable")
            })?
            .ok_or_else(|| {
                ReviewError::new(ReviewErrorCode::Refused, "review runtime is unknown")
            })?;
        if runtime.descriptor().kind() != AgentRuntimeKind::Delegated
            || runtime.descriptor().capabilities().permissions != CapabilitySupport::Supported
        {
            return Err(ReviewError::new(
                ReviewErrorCode::Refused,
                "review runtime cannot prove delegated permission callbacks",
            ));
        }
        self.commit(
            ReviewChange::started(
                request.run_id().clone(),
                request.runtime(),
                request.base().as_str(),
                request.patch(),
                request.instructions(),
            )
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Refused,
                    "review request metadata is invalid",
                )
            })?,
        )?;

        let result = self
            .run_started(runtime, &request, cancellation.clone())
            .await;
        match result {
            Ok(result) => {
                let change = ReviewChange::completed(
                    result.run_id.clone(),
                    result.child_session_id.clone(),
                    result.summary.clone(),
                    result.findings.clone(),
                )
                .map_err(|_| {
                    ReviewError::new(
                        ReviewErrorCode::InvalidOutput,
                        "structured review result is invalid",
                    )
                })?;
                self.commit(change)?;
                Ok(result)
            }
            Err(error) => {
                let failure =
                    ReviewChange::failed(request.run_id().clone(), error.durable_reason())
                        .map_err(|_| {
                            ReviewError::new(ReviewErrorCode::Failed, "review failure is invalid")
                        })?;
                self.commit(failure)?;
                Err(error)
            }
        }
    }

    async fn run_started(
        &self,
        runtime: Arc<dyn heycode_runtime::AgentRuntime>,
        request: &ReviewRequest,
        cancellation: CancellationToken,
    ) -> Result<ReviewResult, ReviewError> {
        let lease = self
            .worktrees
            .create(request.base().clone(), cancellation.clone())
            .await
            .map_err(|_| {
                ReviewError::new(
                    ReviewErrorCode::Workspace,
                    "review worktree could not be created",
                )
            })?;
        if let Err(error) = lease
            .apply_patch(request.patch(), cancellation.clone())
            .await
        {
            let _cleanup = lease
                .finish(WorktreeOutcome::Failure, CancellationToken::new())
                .await;
            let _ = error;
            return Err(ReviewError::new(
                ReviewErrorCode::Workspace,
                "review patch could not be applied",
            ));
        }
        let baseline = match lease.snapshot(CancellationToken::new()).await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                let _cleanup = lease
                    .finish(WorktreeOutcome::Failure, CancellationToken::new())
                    .await;
                return Err(ReviewError::new(
                    ReviewErrorCode::Workspace,
                    "review baseline could not be captured",
                ));
            }
        };
        let provider = match RuntimeSubagentProvider::new(
            runtime,
            Arc::new(DenyAll),
            "isolated-reviewer",
            "Isolated reviewer",
            self.sessions_root.clone(),
            lease.path().to_path_buf(),
            1,
        ) {
            Ok(provider) => provider,
            Err(_) => {
                let _cleanup = lease
                    .finish(WorktreeOutcome::Failure, CancellationToken::new())
                    .await;
                return Err(ReviewError::new(
                    ReviewErrorCode::Runtime,
                    "review runtime could not be prepared",
                ));
            }
        };
        let prompt = reviewer_prompt(request.instructions());
        let subagent_request = match SubagentRequest::new(
            "review",
            prompt,
            SubagentSeed::Fresh,
            SubagentContinuation::OneShot,
            0,
        ) {
            Ok(request) => request,
            Err(_) => {
                let _cleanup = lease
                    .finish(WorktreeOutcome::Failure, CancellationToken::new())
                    .await;
                return Err(ReviewError::new(
                    ReviewErrorCode::Refused,
                    "review prompt is invalid",
                ));
            }
        };
        let operation = CancellationToken::new();
        let mut run = Box::pin(provider.start(subagent_request, operation.clone()));
        let started = tokio::select! {
            result = &mut run => result,
            () = cancellation.cancelled() => {
                operation.cancel();
                run.await
            }
            () = self.shutdown.cancelled() => {
                operation.cancel();
                run.await
            }
        };
        let after = lease.snapshot(CancellationToken::new()).await;
        let mutation = after.as_ref().is_ok_and(|after| after != &baseline);
        let provisional = match started {
            Ok(_started) if mutation => Err(ReviewError::new(
                ReviewErrorCode::MutationDetected,
                "reviewer changed the isolated checkout; findings were rejected",
            )),
            Ok(_started) if after.is_err() => Err(ReviewError::new(
                ReviewErrorCode::Workspace,
                "review mutation audit could not be completed",
            )),
            Ok(started) => {
                parse_review(request.run_id().clone(), started.id.as_str(), &started.text)
            }
            Err(error) if error.code() == crate::SubagentErrorCode::Cancelled => Err(
                ReviewError::new(ReviewErrorCode::Cancelled, "review was cancelled"),
            ),
            Err(_) => Err(ReviewError::new(
                ReviewErrorCode::Runtime,
                "review runtime failed",
            )),
        };
        let outcome = if provisional.is_ok() {
            WorktreeOutcome::Success
        } else if provisional
            .as_ref()
            .is_err_and(|error| error.code() == ReviewErrorCode::Cancelled)
        {
            WorktreeOutcome::Cancelled
        } else {
            WorktreeOutcome::Failure
        };
        lease
            .finish(outcome, CancellationToken::new())
            .await
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::Workspace, "review worktree cleanup failed")
            })?;
        provisional
    }

    fn commit(&self, change: ReviewChange) -> Result<SessionEvent, ReviewError> {
        let mut session = self.session.lock().map_err(|_| {
            ReviewError::new(ReviewErrorCode::Failed, "review session is unavailable")
        })?;
        let kind = SessionEventKind::ReviewChange {
            change: Box::new(change),
        };
        let mut candidate = session.events().to_vec();
        candidate.push(SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: candidate.len() as u64,
            time_ms: 0,
            kind: kind.clone(),
        });
        project_reviews(&candidate).map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "review change conflicts with durable state",
            )
        })?;
        let event = session.append(kind).map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "review change could not be committed",
            )
        })?;
        session.flush().map_err(|_| {
            ReviewError::new(
                ReviewErrorCode::Failed,
                "review change could not be flushed",
            )
        })?;
        Ok(event)
    }
}

fn relative_utf8(path: &Path, root: &Path) -> Result<String, ReviewError> {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ReviewError::new(
                ReviewErrorCode::Refused,
                "finding source path is not a portable workspace path",
            )
        })
}

fn invalid_report() -> ReviewError {
    ReviewError::new(
        ReviewErrorCode::Refused,
        "finding report metadata is invalid",
    )
}

fn report_read_error(error: heycode_exec::FileSystemError) -> ReviewError {
    match error.code() {
        FileSystemErrorCode::Cancelled => {
            ReviewError::new(ReviewErrorCode::Cancelled, "finding report was cancelled")
        }
        FileSystemErrorCode::StaleObservation | FileSystemErrorCode::ChangedAtCommit => {
            ReviewError::new(
                ReviewErrorCode::Refused,
                "a finding file changed since it was read; read it again before reporting",
            )
        }
        FileSystemErrorCode::InvalidSpec => ReviewError::new(
            ReviewErrorCode::Refused,
            "finding line range or revision is invalid",
        ),
        _ => ReviewError::new(
            ReviewErrorCode::Refused,
            "a finding file is unavailable from the active workspace",
        ),
    }
}

/// Static O14 plugin paths.
#[derive(Clone)]
pub struct ReviewPluginConfig {
    repository: PathBuf,
    worktrees_root: PathBuf,
    sessions_root: PathBuf,
}

impl ReviewPluginConfig {
    /// Bind the source repository plus isolated worktree and durable child roots.
    #[must_use]
    pub const fn new(repository: PathBuf, worktrees_root: PathBuf, sessions_root: PathBuf) -> Self {
        Self {
            repository,
            worktrees_root,
            sessions_root,
        }
    }
}

/// Mount the reviewer service, `report_findings`, and the selected-runtime command.
///
/// The existing TUI-owned `/review` remains a separate active-route command
/// until root composition deliberately replaces that UI path.
#[must_use]
pub fn review_plugin(config: ReviewPluginConfig) -> Box<dyn heycode_core::Plugin> {
    struct ReviewPlugin(ReviewPluginConfig);

    impl heycode_core::Plugin for ReviewPlugin {
        fn name(&self) -> &'static str {
            "reviewer"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    "report_findings",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "review-runtime",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_exec::SERVICE_SUBPROCESS,
                heycode_runtime::SERVICE_RUNTIMES,
                heycode_session::SERVICE_SESSION,
                crate::SERVICE_AGENT,
                crate::SERVICE_JOBS,
                crate::SERVICE_COMMANDS,
                crate::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
                heycode_tools::SERVICE_TOOLS,
            ]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_REVIEWS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let subprocess = context
                .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| heycode_core::CoreError::other("subprocess service missing"))?;
            let runtimes = context
                .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
                .ok_or_else(|| heycode_core::CoreError::other("runtime registry missing"))?;
            let session = context
                .get::<Mutex<Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| heycode_core::CoreError::other("session service missing"))?;
            let agent = context
                .get::<Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service missing"))?;
            let jobs = context
                .get::<Arc<JobRegistry>>(crate::SERVICE_JOBS)
                .ok_or_else(|| heycode_core::CoreError::other("job registry missing"))?;
            let commands = context
                .get::<crate::CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| heycode_core::CoreError::other("command registry missing"))?;
            let workspace = context
                .get::<crate::workspace_transition::WorkspaceTransitionHandle>(
                    crate::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
                )
                .ok_or_else(|| heycode_core::CoreError::other("workspace owner missing"))?;
            let tools = context
                .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| heycode_core::CoreError::other("tool registry missing"))?;
            let worktrees = Arc::new(
                GitWorktreeManager::new(
                    (*subprocess).clone(),
                    self.0.repository.clone(),
                    self.0.worktrees_root.clone(),
                    WorktreeRetention::RemoveAlways,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
            );
            let service = ReviewService::new(
                session,
                runtimes,
                worktrees.clone(),
                self.0.sessions_root.clone(),
            );
            service
                .attach_job_host(&agent, (*jobs).clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            service
                .attach_finding_host(&workspace.0, agent.ui().clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.provide(crate::SERVICE_REVIEWS, self.name(), service)?;
            let service = context
                .get::<ReviewService>(crate::SERVICE_REVIEWS)
                .ok_or_else(|| heycode_core::CoreError::other("review service missing"))?;
            let registration = tools
                .register_owned(Arc::new(ReportFindingsTool {
                    service: service.clone(),
                }))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || drop(registration));
            let command = ReviewRuntimeCommand::new(service.clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            commands
                .register_effect(context, Arc::new(command))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || worktrees.dispose());
            context.effect(move || service.dispose());
            Ok(())
        }
    }

    Box::new(ReviewPlugin(config))
}

struct ReviewRuntimeCommand {
    descriptor: crate::CommandDescriptor,
    service: Arc<ReviewService>,
    unavailable_no_runtime: crate::CommandAvailability,
    unavailable_registry: crate::CommandAvailability,
}

struct ReportFindingsTool {
    service: Arc<ReviewService>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFindingReport {
    level: Option<WireReviewLevel>,
    findings: Vec<WireReportedFinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReportedFinding {
    severity: WireSeverity,
    path: String,
    line_start: u32,
    line_end: u32,
    revision: String,
    title: String,
    trigger: String,
    failure: String,
    impact: String,
    category: Option<String>,
    verdict: Option<WireVerificationVerdict>,
    outcome: Option<WireFindingOutcome>,
}

#[async_trait]
impl heycode_tools::Tool for ReportFindingsTool {
    fn prerequisite_status(&self) -> heycode_tools::ToolPrerequisiteStatus {
        heycode_tools::ToolPrerequisiteStatus {
            configured: Some(true),
            detail: "Session, workspace authority, revision checks, and local UI presentation are composed; external publication is not part of this tool.".to_owned(),
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "report_findings".to_owned(),
            description: "Report one or more concrete findings against files you just read. Each finding must include the exact 64-character revision returned by read plus the same cwd-relative read path, one-based line range, severity, trigger, failure, and impact. Optional review level, category, verification verdict, and post-fix outcome preserve additional review context. heycode normalizes the path to its workspace root, rechecks every file revision, and derives the session/workspace source identity before one durable local commit. This does not publish or send findings externally.".to_owned(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "level":{"enum":["low","medium","high","xhigh","max"],"description":"Optional effort level used for this review."},
                    "findings":{
                        "type":"array",
                        "minItems":1,
                        "maxItems":MAX_TOOL_REPORTED_FINDINGS,
                        "items":{
                            "type":"object",
                            "properties":{
                                "severity":{"enum":["critical","high","medium","low"],"description":"Impact-calibrated severity of this concrete defect."},
                                "path":{"type":"string","minLength":1,"maxLength":4096,"description":"The same relative path used with read, resolved from the current working directory."},
                                "line_start":{"type":"integer","minimum":1,"maximum":4294967295_u64,"description":"First affected one-based line."},
                                "line_end":{"type":"integer","minimum":1,"maximum":4294967295_u64,"description":"Last affected one-based line, at or after line_start."},
                                "revision":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Exact revision returned by read for this file."},
                                "title":{"type":"string","minLength":1,"maxLength":256,"description":"Short actionable defect title."},
                                "trigger":{"type":"string","minLength":1,"maxLength":8192,"description":"Concrete preconditions and actions that reproduce the problem."},
                                "failure":{"type":"string","minLength":1,"maxLength":8192,"description":"Incorrect behavior or invariant violation that occurs."},
                                "impact":{"type":"string","minLength":1,"maxLength":8192,"description":"User, security, correctness, or operational consequence."},
                                "category":{"type":"string","minLength":1,"maxLength":40,"pattern":"^[a-z0-9]+(?:-[a-z0-9]+)*$","description":"Optional lower-case kebab-case finding type, such as correctness or test-coverage."},
                                "verdict":{"enum":["CONFIRMED","PLAUSIBLE"],"description":"Set only when an explicit verification pass ran."},
                                "outcome":{"enum":["fixed","skipped","no_change_needed"],"description":"Set only when re-reporting after an explicitly requested fix pass."}
                            },
                            "required":["severity","path","line_start","line_end","revision","title","trigger","failure","impact"],
                            "additionalProperties":false
                        }
                    }
                },
                "required":["findings"],
                "additionalProperties":false
            }),
        }
    }

    async fn run(
        &self,
        args: Value,
        cx: &heycode_tools::ToolCtx,
    ) -> Result<Value, heycode_tools::ToolError> {
        let (level, findings) = parse_finding_report_args(args)?;
        let report = self
            .service
            .report_findings_with_level(level, findings, &cx.cwd, cx.cancellation.clone())
            .await
            .map_err(|error| heycode_tools::ToolError::new(error.to_string()))?;
        Ok(json!({
            "report_id": report.id().as_str(),
            "source": {
                "session_id": report.source().session_id().as_str(),
                "workspace_revision": report.source().workspace_revision(),
                "root_index": report.source().root_index(),
                "cwd": report.source().cwd(),
            },
            "findings_count": report.findings().len(),
            "published": false,
        }))
    }
}

fn parse_finding_report_args(
    args: Value,
) -> Result<(Option<ReviewLevel>, Vec<ReportedFinding>), heycode_tools::ToolError> {
    let wire: WireFindingReport = serde_json::from_value(args).map_err(|_| {
        heycode_tools::ToolError::new("report_findings arguments do not match the strict schema")
    })?;
    if !(1..=MAX_TOOL_REPORTED_FINDINGS).contains(&wire.findings.len()) {
        return Err(heycode_tools::ToolError::new(
            "report_findings requires between 1 and 32 findings",
        ));
    }
    let level = wire.level.map(|level| match level {
        WireReviewLevel::Low => ReviewLevel::Low,
        WireReviewLevel::Medium => ReviewLevel::Medium,
        WireReviewLevel::High => ReviewLevel::High,
        WireReviewLevel::Xhigh => ReviewLevel::Xhigh,
        WireReviewLevel::Max => ReviewLevel::Max,
    });
    let findings = wire
        .findings
        .into_iter()
        .enumerate()
        .map(|(index, finding)| {
            let severity = match finding.severity {
                WireSeverity::Critical => ReviewSeverity::Critical,
                WireSeverity::High => ReviewSeverity::High,
                WireSeverity::Medium => ReviewSeverity::Medium,
                WireSeverity::Low => ReviewSeverity::Low,
            };
            ReportedFinding::new(
                format!("finding-{}", index + 1),
                severity,
                finding.path,
                finding.line_start,
                finding.line_end,
                finding.revision,
                finding.title,
                finding.trigger,
                finding.failure,
                finding.impact,
            )
            .and_then(|reported| {
                reported.with_reference_dimensions(
                    finding.category,
                    finding.verdict.map(|verdict| match verdict {
                        WireVerificationVerdict::Confirmed => FindingVerificationVerdict::Confirmed,
                        WireVerificationVerdict::Plausible => FindingVerificationVerdict::Plausible,
                    }),
                    finding.outcome.map(|outcome| match outcome {
                        WireFindingOutcome::Fixed => FindingOutcome::Fixed,
                        WireFindingOutcome::Skipped => FindingOutcome::Skipped,
                        WireFindingOutcome::NoChangeNeeded => FindingOutcome::NoChangeNeeded,
                    }),
                )
            })
            .map_err(|_| heycode_tools::ToolError::new("finding report metadata is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((level, findings))
}

impl ReviewRuntimeCommand {
    fn new(service: Arc<ReviewService>) -> Result<Self, crate::CommandMetadataError> {
        Ok(Self {
            descriptor: crate::CommandDescriptor::new(
                "review-runtime",
                "Review tracked workspace changes with a selected isolated runtime",
                vec![
                    crate::CommandArgument::required("runtime", "Delegated runtime id")?,
                    crate::CommandArgument::optional(
                        "instructions",
                        "Additional review instructions",
                    )?
                    .variadic(),
                ],
                crate::CommandTiming::ModelScheduling,
                crate::CommandSource::from_plugin("reviewer")?,
            )?,
            service,
            unavailable_no_runtime: crate::CommandAvailability::unavailable(
                "no delegated runtime proves reviewer permission callbacks",
            )?,
            unavailable_registry: crate::CommandAvailability::unavailable(
                "runtime registry unavailable",
            )?,
        })
    }
}

#[async_trait]
impl crate::Command for ReviewRuntimeCommand {
    fn descriptor(&self) -> &crate::CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> crate::CommandAvailability {
        match self.service.selectable_runtimes() {
            Ok(rows) if !rows.is_empty() => crate::CommandAvailability::available(),
            Ok(_) => self.unavailable_no_runtime.clone(),
            Err(_) => self.unavailable_registry.clone(),
        }
    }

    async fn execute(&self, _agent: &Agent, args: &str) -> anyhow::Result<()> {
        let trimmed = args.trim();
        let (runtime, instructions) = trimmed
            .split_once(char::is_whitespace)
            .map_or((trimmed, ""), |(runtime, instructions)| {
                (runtime, instructions.trim())
            });
        if runtime.is_empty() {
            anyhow::bail!("usage: /review-runtime <runtime> [instructions]");
        }
        self.service
            .start_background(runtime, instructions)
            .map_err(anyhow::Error::new)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReview {
    summary: String,
    findings: Vec<WireFinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFinding {
    id: String,
    severity: WireSeverity,
    path: String,
    line_start: u32,
    line_end: u32,
    title: String,
    body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSeverity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireReviewLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum WireVerificationVerdict {
    Confirmed,
    Plausible,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireFindingOutcome {
    Fixed,
    Skipped,
    NoChangeNeeded,
}

fn parse_review(
    run_id: ReviewRunId,
    child_session_id: &str,
    text: &str,
) -> Result<ReviewResult, ReviewError> {
    let wire: WireReview = serde_json::from_str(text).map_err(|_| {
        ReviewError::new(
            ReviewErrorCode::InvalidOutput,
            "review output does not match the structured schema",
        )
    })?;
    let findings = wire
        .findings
        .into_iter()
        .map(|finding| {
            ReviewFinding::new(
                finding.id,
                match finding.severity {
                    WireSeverity::Critical => ReviewSeverity::Critical,
                    WireSeverity::High => ReviewSeverity::High,
                    WireSeverity::Medium => ReviewSeverity::Medium,
                    WireSeverity::Low => ReviewSeverity::Low,
                },
                finding.path,
                finding.line_start,
                finding.line_end,
                finding.title,
                finding.body,
            )
            .map_err(|_| {
                ReviewError::new(ReviewErrorCode::InvalidOutput, "review finding is invalid")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    ReviewChange::completed(
        run_id.clone(),
        child_session_id,
        wire.summary.clone(),
        findings.clone(),
    )
    .map_err(|_| ReviewError::new(ReviewErrorCode::InvalidOutput, "review result is invalid"))?;
    Ok(ReviewResult {
        run_id,
        child_session_id: child_session_id.to_owned(),
        summary: wire.summary,
        findings,
    })
}

fn reviewer_prompt(instructions: &str) -> String {
    let suffix = if instructions.is_empty() {
        String::new()
    } else {
        format!("\n\nAdditional instructions:\n{instructions}")
    };
    format!(
        "Review the tracked changes already applied in this isolated checkout. Do not modify files, the index, refs, or configuration. Return only one JSON object with exactly this shape: {{\"summary\":\"...\",\"findings\":[{{\"id\":\"finding-1\",\"severity\":\"critical|high|medium|low\",\"path\":\"relative/path.rs\",\"line_start\":1,\"line_end\":1,\"title\":\"...\",\"body\":\"...\"}}]}}. Use an empty findings array when there are no actionable defects.{suffix}"
    )
}

fn render_notice(result: &ReviewResult) -> String {
    let mut output = format!("review {}: {}", result.run_id(), result.summary());
    for finding in result.findings() {
        let severity = match finding.severity() {
            ReviewSeverity::Critical => "critical",
            ReviewSeverity::High => "high",
            ReviewSeverity::Medium => "medium",
            ReviewSeverity::Low => "low",
        };
        output.push_str(&format!(
            "\n- [{severity}] {}:{} {}",
            finding.path(),
            finding.line_start(),
            finding.title()
        ));
        if output.len() > MAX_REVIEW_NOTICE_BYTES {
            break;
        }
    }
    bounded(&output, MAX_REVIEW_NOTICE_BYTES)
}

fn bounded(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod finding_report_tool_tests {
    use super::*;

    fn required_finding() -> Value {
        json!({
            "severity": "high",
            "path": "src/lib.rs",
            "line_start": 4,
            "line_end": 5,
            "revision": "a".repeat(64),
            "title": "Unchecked boundary",
            "trigger": "Call the parser with a truncated frame.",
            "failure": "The length subtraction underflows.",
            "impact": "The session terminates and drops pending work."
        })
    }

    #[test]
    fn report_tool_parses_optional_reference_dimensions_and_old_defaults() {
        let mut finding = required_finding();
        finding["category"] = json!("test-coverage");
        finding["verdict"] = json!("CONFIRMED");
        finding["outcome"] = json!("no_change_needed");
        let (level, findings) = parse_finding_report_args(json!({
            "level": "xhigh",
            "findings": [finding]
        }))
        .expect("reference-compatible dimensions should parse");
        assert_eq!(level, Some(ReviewLevel::Xhigh));
        assert_eq!(findings[0].category(), Some("test-coverage"));
        assert_eq!(
            findings[0].verdict(),
            Some(FindingVerificationVerdict::Confirmed)
        );
        assert_eq!(findings[0].outcome(), Some(FindingOutcome::NoChangeNeeded));

        let (level, findings) =
            parse_finding_report_args(json!({"findings": [required_finding()]}))
                .expect("the previous required-only shape should remain valid");
        assert_eq!(level, None);
        assert_eq!(findings[0].category(), None);
        assert_eq!(findings[0].verdict(), None);
        assert_eq!(findings[0].outcome(), None);
    }

    #[test]
    fn report_tool_rejects_unadvertised_dimension_values() {
        let invalid = [
            json!({"level": "extra", "findings": [required_finding()]}),
            json!({"findings": [{
                "severity": "high",
                "path": "src/lib.rs",
                "line_start": 4,
                "line_end": 5,
                "revision": "a".repeat(64),
                "title": "Unchecked boundary",
                "trigger": "Call the parser with a truncated frame.",
                "failure": "The length subtraction underflows.",
                "impact": "The session terminates and drops pending work.",
                "category": "not_valid"
            }]}),
            json!({"findings": [{
                "severity": "high",
                "path": "src/lib.rs",
                "line_start": 4,
                "line_end": 5,
                "revision": "a".repeat(64),
                "title": "Unchecked boundary",
                "trigger": "Call the parser with a truncated frame.",
                "failure": "The length subtraction underflows.",
                "impact": "The session terminates and drops pending work.",
                "verdict": "confirmed"
            }]}),
            json!({"findings": [{
                "severity": "high",
                "path": "src/lib.rs",
                "line_start": 4,
                "line_end": 5,
                "revision": "a".repeat(64),
                "title": "Unchecked boundary",
                "trigger": "Call the parser with a truncated frame.",
                "failure": "The length subtraction underflows.",
                "impact": "The session terminates and drops pending work.",
                "outcome": "changed"
            }]}),
        ];
        assert!(
            invalid
                .into_iter()
                .all(|args| parse_finding_report_args(args).is_err())
        );
    }

    #[test]
    fn report_tool_accepts_32_findings_and_refuses_33() {
        let findings = |count| {
            (0..count)
                .map(|index| {
                    let mut finding = required_finding();
                    finding["failure"] = json!(format!("Distinct failure {index}."));
                    finding
                })
                .collect::<Vec<_>>()
        };
        assert!(
            parse_finding_report_args(json!({"findings": findings(32)})).is_ok(),
            "the reference-compatible maximum should be admitted"
        );
        assert!(
            parse_finding_report_args(json!({"findings": findings(33)})).is_err(),
            "new tool calls above the reference-compatible maximum must fail"
        );
    }
}
