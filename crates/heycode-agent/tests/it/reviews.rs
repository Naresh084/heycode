#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_agent::workspace_transition::WorkspaceTransitionService;
use heycode_agent::{
    DenyAll, GitCommitId, GitWorktreeManager, ReviewErrorCode, ReviewService, SubagentContinuation,
    SubagentId, SubagentRegistry, SubagentRequest, SubagentSeed, UiEvent, WorktreeRetention,
    WorktreeRuntimeSubagentProvider,
};
use heycode_core::EventBus;
use heycode_exec::{
    LocalShellConfig, PathRequest, ReadFileSpec, SandboxMode, SandboxService, ShellService,
    SubprocessService,
};
use heycode_llm::CapabilitySupport;
use heycode_runtime::{
    AccountState, AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeKind, RuntimeCapabilities,
    RuntimeCompactOutcome, RuntimeEvent, RuntimeEventKind, RuntimeEventStream, RuntimeFinishReason,
    RuntimeFork, RuntimeInput, RuntimePermissionResponse, RuntimeQuestionResponse, RuntimeResume,
    RuntimeSession, RuntimeSessionId, RuntimeStart, RuntimeTurnId,
};
use heycode_session::{
    FindingOutcome, FindingReport, FindingVerificationVerdict, ReportedFinding,
    ReviewFailureReason, ReviewLevel, ReviewSeverity, ReviewState, Session, project_reviews,
};
use tokio_util::sync::CancellationToken;

fn git(repository: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(root: &std::path::Path) -> std::path::PathBuf {
    let repository = root.join("repository");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "-q"]);
    std::fs::write(repository.join("src.rs"), "fn value() -> u32 { 1 }\n").unwrap();
    git(&repository, &["add", "--", "src.rs"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=heycode test",
            "-c",
            "user.email=heycode@example.invalid",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    std::fs::write(repository.join("src.rs"), "fn value() -> u32 { 2 }\n").unwrap();
    repository
}

struct ScriptedRuntime {
    descriptor: AgentRuntimeDescriptor,
    output: String,
    mutate: bool,
}

#[async_trait::async_trait]
impl AgentRuntime for ScriptedRuntime {
    fn descriptor(&self) -> &AgentRuntimeDescriptor {
        &self.descriptor
    }

    async fn account(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<AccountState, heycode_runtime::RuntimeError> {
        Ok(AccountState::without_label(
            heycode_runtime::AccountStatus::NotRequired,
        ))
    }

    async fn models(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn start(
        &self,
        request: RuntimeStart,
        _cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
        Ok(Arc::new(ScriptedSession {
            runtime_id: self.descriptor.id().clone(),
            session_id: RuntimeSessionId::new("review-session").unwrap(),
            capabilities: self.descriptor.capabilities().clone(),
            workspace: request.workspace().to_path_buf(),
            output: self.output.clone(),
            mutate: self.mutate,
        }))
    }

    async fn resume(
        &self,
        _request: RuntimeResume,
        _cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn fork(
        &self,
        _request: RuntimeFork,
        _cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }
}

struct ScriptedSession {
    runtime_id: heycode_runtime::AgentRuntimeId,
    session_id: RuntimeSessionId,
    capabilities: RuntimeCapabilities,
    workspace: std::path::PathBuf,
    output: String,
    mutate: bool,
}

#[async_trait::async_trait]
impl RuntimeSession for ScriptedSession {
    fn id(&self) -> &RuntimeSessionId {
        &self.session_id
    }

    fn runtime_id(&self) -> &heycode_runtime::AgentRuntimeId {
        &self.runtime_id
    }

    fn capabilities(&self) -> &RuntimeCapabilities {
        &self.capabilities
    }

    fn subscribe(&self) -> RuntimeEventStream {
        let turn = RuntimeTurnId::new("turn-1").unwrap();
        Box::pin(futures::stream::iter(vec![
            Ok(RuntimeEvent::new(0, RuntimeEventKind::SessionReady)),
            Ok(RuntimeEvent::new(
                1,
                RuntimeEventKind::TurnStarted { turn: turn.clone() },
            )),
            Ok(RuntimeEvent::new(
                2,
                RuntimeEventKind::FinalMessage {
                    text: self.output.clone(),
                },
            )),
            Ok(RuntimeEvent::new(
                3,
                RuntimeEventKind::TurnFinished {
                    turn,
                    reason: RuntimeFinishReason::Stop,
                },
            )),
        ]))
    }

    async fn send(
        &self,
        _input: RuntimeInput,
        _cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, heycode_runtime::RuntimeError> {
        if self.mutate {
            std::fs::write(self.workspace.join("mutation.txt"), "not allowed\n")
                .map_err(|_| heycode_runtime::RuntimeError::internal("fixture mutation"))?;
        }
        RuntimeTurnId::new("turn-1").map_err(|_| heycode_runtime::RuntimeError::invalid_request())
    }

    async fn steer(
        &self,
        _input: RuntimeInput,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn follow_up(
        &self,
        _input: RuntimeInput,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn cancel(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Ok(())
    }

    async fn respond_permission(
        &self,
        _response: RuntimePermissionResponse,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Ok(())
    }

    async fn respond_question(
        &self,
        _response: RuntimeQuestionResponse,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn compact(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, heycode_runtime::RuntimeError> {
        Err(heycode_runtime::RuntimeError::unsupported())
    }

    async fn close(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<(), heycode_runtime::RuntimeError> {
        Ok(())
    }
}

fn descriptor() -> AgentRuntimeDescriptor {
    AgentRuntimeDescriptor::new(
        "fixture-reviewer",
        "Fixture reviewer",
        AgentRuntimeKind::Delegated,
        RuntimeCapabilities {
            models: CapabilitySupport::Unsupported,
            resume: CapabilitySupport::Unsupported,
            fork: CapabilitySupport::Unsupported,
            steer: CapabilitySupport::Unsupported,
            follow_up: CapabilitySupport::Unsupported,
            permissions: CapabilitySupport::Supported,
            questions: CapabilitySupport::Unsupported,
            compaction: CapabilitySupport::Unsupported,
        },
    )
    .unwrap()
}

fn service(
    root: &std::path::Path,
    output: &str,
    mutate: bool,
) -> (
    heycode_core::Context,
    Arc<std::sync::Mutex<Session>>,
    Arc<ReviewService>,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let repository = repository(root);
    let context = heycode_core::Context::new();
    let runtimes = Arc::new(heycode_runtime::AgentRuntimeRegistry::new());
    runtimes
        .register(
            &context,
            Arc::new(ScriptedRuntime {
                descriptor: descriptor(),
                output: output.to_owned(),
                mutate,
            }),
        )
        .unwrap();
    let session = Arc::new(std::sync::Mutex::new(
        Session::create(root.join("main-sessions")).unwrap(),
    ));
    let worktrees_root = root.join("review-worktrees");
    let manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository.clone(),
            worktrees_root.clone(),
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let service = Arc::new(ReviewService::new(
        session.clone(),
        runtimes,
        manager,
        root.join("child-sessions"),
    ));
    (context, session, service, worktrees_root, repository)
}

#[tokio::test]
async fn selected_runtime_emits_structured_findings_after_unchanged_workspace_audit() {
    let root = tempfile::tempdir().unwrap();
    let output = serde_json::json!({
        "summary":"One issue",
        "findings":[{
            "id":"finding-1",
            "severity":"high",
            "path":"src.rs",
            "line_start":1,
            "line_end":1,
            "title":"Changed return value",
            "body":"The new value violates the fixture contract."
        }]
    })
    .to_string();
    let (mut context, session, service, worktrees_root, _repository) =
        service(root.path(), &output, false);
    assert_eq!(
        service.selectable_runtimes().unwrap(),
        vec!["fixture-reviewer"]
    );
    let result = service
        .review_workspace(
            "fixture-reviewer",
            "Focus on correctness",
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.summary(), "One issue");
    assert_eq!(result.findings().len(), 1);
    let projection = project_reviews(session.lock().unwrap().events()).unwrap();
    let run = projection.run(result.run_id()).unwrap();
    assert_eq!(run.state(), ReviewState::Completed);
    assert!(run.patch().contains("+fn value() -> u32 { 2 }"));
    let retained = std::fs::read_dir(worktrees_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().unwrap().is_dir() && entry.file_name().to_string_lossy().len() == 36
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(
        retained.len(),
        1,
        "changed worktree contents must remain recoverable"
    );
    assert_eq!(
        std::fs::read_to_string(retained[0].join("src.rs")).unwrap(),
        "fn value() -> u32 { 2 }\n"
    );
    assert!(!retained[0].join("mutation.txt").exists());
    context.shutdown();
}

#[tokio::test]
async fn reviewer_mutation_is_rejected_and_commits_no_findings() {
    let root = tempfile::tempdir().unwrap();
    let output = serde_json::json!({"summary":"clean","findings":[]}).to_string();
    let (mut context, session, service, worktrees_root, _repository) =
        service(root.path(), &output, true);
    let error = service
        .review_workspace(
            "fixture-reviewer",
            "Do not mutate",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), ReviewErrorCode::MutationDetected);
    let projection = project_reviews(session.lock().unwrap().events()).unwrap();
    let run = projection.runs()[0];
    assert_eq!(
        run.state(),
        ReviewState::Failed(ReviewFailureReason::MutationDetected)
    );
    assert!(run.findings().is_empty());
    let retained = std::fs::read_dir(worktrees_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().unwrap().is_dir() && entry.file_name().to_string_lossy().len() == 36
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(
        retained.len(),
        1,
        "changed worktree contents must remain recoverable"
    );
    assert_eq!(
        std::fs::read_to_string(retained[0].join("mutation.txt")).unwrap(),
        "not allowed\n"
    );
    context.shutdown();
}

#[tokio::test]
async fn worktree_runtime_provider_preserves_result_and_reports_its_location() {
    let root = tempfile::tempdir().unwrap();
    let repository = repository(root.path());
    let base = GitCommitId::new(git(&repository, &["rev-parse", "HEAD"])).unwrap();
    let worktrees_root = root.path().join("worktrees");
    let manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository,
            worktrees_root.clone(),
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let provider = Arc::new(
        WorktreeRuntimeSubagentProvider::new(
            Arc::new(ScriptedRuntime {
                descriptor: descriptor(),
                output: "done in worktree".to_owned(),
                mutate: true,
            }),
            Arc::new(DenyAll),
            "worktree-fixture",
            "Worktree fixture",
            root.path().join("sessions"),
            manager,
            base,
            2,
        )
        .unwrap(),
    );
    let registry = SubagentRegistry::new();
    registry.register(provider).unwrap();
    let authority = registry.root_authority(SubagentId::new("root").unwrap());
    let started = registry
        .start(
            SubagentRequest::with_authority(
                "isolated",
                "work in the exact-base checkout",
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
                authority,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        started
            .text
            .starts_with("done in worktree\n\n[worktree results retained: ")
    );
    let retained = std::fs::read_dir(worktrees_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().unwrap().is_dir() && entry.file_name().to_string_lossy().len() == 36
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(
        retained.len(),
        1,
        "changed worktree contents must remain recoverable"
    );
    assert!(started.text.contains(&retained[0].display().to_string()));
    assert_eq!(
        std::fs::read_to_string(retained[0].join("mutation.txt")).unwrap(),
        "not allowed\n"
    );
}

fn report_workspace(
    root: &std::path::Path,
    repository: &std::path::Path,
) -> Arc<WorkspaceTransitionService> {
    let root = std::fs::canonicalize(root).unwrap();
    let state = root.join("report-workspace-state");
    std::fs::create_dir(&state).unwrap();
    let state = std::fs::canonicalize(state).unwrap();
    let sandbox = SandboxService::new(SandboxMode::Off, repository, None).unwrap();
    let shell = ShellService::local(
        LocalShellConfig::platform(repository.to_path_buf(), Duration::from_secs(5)).unwrap(),
    );
    WorkspaceTransitionService::open(
        state.join("workspace.json"),
        repository.to_path_buf(),
        sandbox,
        shell,
        SubprocessService::local(),
        root.join("report-workspace-worktrees"),
    )
    .unwrap()
}

async fn file_revision(workspace: &WorkspaceTransitionService, cwd: &std::path::Path) -> String {
    let filesystem = workspace.pinned_filesystem().unwrap();
    let path = filesystem
        .resolve(PathRequest::new(cwd, "src.rs").unwrap())
        .unwrap();
    filesystem
        .read(
            ReadFileSpec::new(path, 16)
                .unwrap()
                .with_window(1, 1, None)
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .page()
        .unwrap()
        .revision
        .clone()
}

fn reported(revision: &str) -> ReportedFinding {
    ReportedFinding::new(
        "finding-1",
        ReviewSeverity::High,
        "src.rs",
        1,
        1,
        revision,
        "Changed return value",
        "Call value after applying the tracked change.",
        "The function returns 2 instead of the required 1.",
        "Callers observe a contract-breaking value.",
    )
    .unwrap()
}

#[tokio::test]
async fn report_findings_verifies_revisions_then_commits_before_ui_presentation() {
    let root = tempfile::tempdir().unwrap();
    let (mut context, session, service, _review_worktrees, repository) =
        service(root.path(), "{}", false);
    let workspace = report_workspace(root.path(), &repository);
    let bus = EventBus::default();
    let presented = Arc::new(Mutex::new(Vec::<FindingReport>::new()));
    let observed = presented.clone();
    bus.on::<UiEvent>(move |event| {
        if let UiEvent::FindingsReported { report } = event {
            observed.lock().unwrap().push(report.clone());
        }
    });
    service.attach_finding_host(&workspace, bus).unwrap();
    let revision = file_revision(&workspace, &repository).await;
    let finding = reported(&revision)
        .with_reference_dimensions(
            Some("correctness".to_owned()),
            Some(FindingVerificationVerdict::Confirmed),
            Some(FindingOutcome::Fixed),
        )
        .unwrap();
    let report = service
        .report_findings_with_level(
            Some(ReviewLevel::High),
            vec![finding],
            &repository,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(report.source().session_id(), session.lock().unwrap().id());
    assert_eq!(report.source().workspace_revision(), 0);
    assert_eq!(report.source().root_index(), 0);
    assert_eq!(report.source().cwd(), None);
    assert_eq!(report.level(), Some(ReviewLevel::High));
    assert_eq!(report.findings()[0].path(), "src.rs");
    assert_eq!(report.findings()[0].revision(), revision);
    assert_eq!(report.findings()[0].category(), Some("correctness"));
    assert_eq!(
        report.findings()[0].verdict(),
        Some(FindingVerificationVerdict::Confirmed)
    );
    assert_eq!(report.findings()[0].outcome(), Some(FindingOutcome::Fixed));
    let durable = project_reviews(session.lock().unwrap().events()).unwrap();
    assert_eq!(durable.reports(), std::slice::from_ref(&report));
    assert_eq!(*presented.lock().unwrap(), vec![report]);
    context.shutdown();
}

#[tokio::test]
async fn report_findings_refuses_stale_foreign_and_cancelled_sources_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let (mut context, session, service, _review_worktrees, repository) =
        service(root.path(), "{}", false);
    let workspace = report_workspace(root.path(), &repository);
    let bus = EventBus::default();
    let presentations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = presentations.clone();
    bus.on::<UiEvent>(move |event| {
        if matches!(event, UiEvent::FindingsReported { .. }) {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    service.attach_finding_host(&workspace, bus).unwrap();
    let revision = file_revision(&workspace, &repository).await;

    std::fs::write(repository.join("src.rs"), "fn value() -> u32 { 3 }\n").unwrap();
    let stale = service
        .report_findings(
            vec![reported(&revision)],
            &repository,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code(), ReviewErrorCode::Refused);

    let foreign = tempfile::tempdir().unwrap();
    let refused = service
        .report_findings(
            vec![reported(&revision)],
            foreign.path(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code(), ReviewErrorCode::Refused);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = service
        .report_findings(vec![reported(&revision)], &repository, cancelled)
        .await
        .unwrap_err();
    assert_eq!(error.code(), ReviewErrorCode::Cancelled);
    assert!(
        project_reviews(session.lock().unwrap().events())
            .unwrap()
            .reports()
            .is_empty()
    );
    assert_eq!(presentations.load(std::sync::atomic::Ordering::SeqCst), 0);
    context.shutdown();
}
