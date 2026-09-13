//! U01/K12 workspace identity, persistence, CAS and project-gate contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::PluginScope;
use heycode_trust::{
    ExplicitWorkspaceTrust, ProjectAccess, ProjectContentPolicy, ProjectInputKind, TrustFrontend,
    TrustPersistence, TrustStartupState, UntrustedProjectAccess, WorkspaceTrustAction,
    WorkspaceTrustActionOutcome, WorkspaceTrustDecision, WorkspaceTrustError,
    WorkspaceTrustService,
};

fn strict() -> ProjectContentPolicy {
    ProjectContentPolicy::new(UntrustedProjectAccess::Block, UntrustedProjectAccess::Block)
}

#[test]
fn canonical_workspace_identity_is_stable_and_unknown_by_default() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::memory(&workspace, strict()).unwrap();
    let snapshot = service.snapshot().unwrap();
    assert_eq!(
        snapshot.identity().canonical_root(),
        workspace.canonicalize().unwrap()
    );
    assert_eq!(snapshot.identity().id().as_str().len(), 64);
    assert!(
        snapshot
            .identity()
            .id()
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
    assert_eq!(snapshot.decision(), WorkspaceTrustDecision::Unknown);
    assert_eq!(snapshot.persistence(), TrustPersistence::None);
    assert_eq!(snapshot.revision(), 0);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&workspace, root.path().join("alias")).unwrap();
        let alias = WorkspaceTrustService::memory(root.path().join("alias"), strict()).unwrap();
        assert_eq!(
            alias.snapshot().unwrap().identity().id(),
            snapshot.identity().id()
        );
    }
}

#[test]
fn terminal_direction_controls_cannot_enter_workspace_identity_or_trust_ui() {
    let root = tempfile::tempdir().unwrap();
    let deceptive = root.path().join("safe-\u{202e}txt");
    std::fs::create_dir(&deceptive).unwrap();
    assert!(matches!(
        WorkspaceTrustService::memory(deceptive, strict()),
        Err(WorkspaceTrustError::InvalidWorkspace)
    ));
}

#[test]
fn persistent_decision_survives_restart_reset_and_stale_cas_never_publish() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let store = home.join("trust.toml");
    std::fs::create_dir(&workspace).unwrap();

    let service = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    let trusted = service.persist(WorkspaceTrustDecision::Trusted, 0).unwrap();
    assert_eq!(trusted.decision(), WorkspaceTrustDecision::Trusted);
    assert_eq!(trusted.persistence(), TrustPersistence::Persistent);
    assert_eq!(trusted.revision(), 1);

    let reopened = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    let restored = reopened.snapshot().unwrap();
    assert_eq!(restored.decision(), WorkspaceTrustDecision::Trusted);
    assert_eq!(restored.persistence(), TrustPersistence::Persistent);
    assert_eq!(restored.revision(), 1);
    assert!(matches!(
        reopened.reset(0),
        Err(WorkspaceTrustError::Conflict {
            expected: 0,
            actual: 1
        })
    ));
    assert_eq!(
        reopened.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Trusted
    );

    let reset = reopened.reset(1).unwrap();
    assert_eq!(reset.decision(), WorkspaceTrustDecision::Unknown);
    assert_eq!(reset.persistence(), TrustPersistence::None);
    assert_eq!(reset.revision(), 2);
    let after_reset = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    assert_eq!(
        after_reset.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
    assert_eq!(after_reset.snapshot().unwrap().revision(), 2);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn session_decisions_never_become_durable() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    let session = service
        .set_session(WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    assert_eq!(session.persistence(), TrustPersistence::Session);
    assert!(!store.exists());

    let reopened = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
}

#[test]
fn executable_project_inputs_and_project_scopes_require_trust() {
    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    for kind in [
        ProjectInputKind::Plugin,
        ProjectInputKind::Process,
        ProjectInputKind::Mcp,
        ProjectInputKind::Hook,
    ] {
        assert!(matches!(
            service.access(kind).unwrap(),
            ProjectAccess::Deferred { .. }
        ));
    }
    for scope in [PluginScope::Project, PluginScope::LocalProject] {
        assert!(!service.allows_plugin_scope(scope).unwrap());
    }
    for scope in [
        PluginScope::BuiltIn,
        PluginScope::User,
        PluginScope::Session,
        PluginScope::Managed,
    ] {
        assert!(service.allows_plugin_scope(scope).unwrap());
    }

    service
        .set_session(WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    for kind in [
        ProjectInputKind::Plugin,
        ProjectInputKind::Process,
        ProjectInputKind::Mcp,
        ProjectInputKind::Hook,
    ] {
        assert_eq!(service.access(kind).unwrap(), ProjectAccess::Allowed);
    }
    assert!(service.allows_plugin_scope(PluginScope::Project).unwrap());
    assert!(
        service
            .allows_plugin_scope(PluginScope::LocalProject)
            .unwrap()
    );
}

#[test]
fn non_executable_project_content_follows_the_explicit_policy() {
    let root = tempfile::tempdir().unwrap();
    let policy = ProjectContentPolicy::new(
        UntrustedProjectAccess::ReadOnly,
        UntrustedProjectAccess::Block,
    );
    let service = WorkspaceTrustService::memory(root.path(), policy).unwrap();
    assert!(matches!(
        service.access(ProjectInputKind::Instructions).unwrap(),
        ProjectAccess::Deferred { .. }
    ));
    assert!(matches!(
        service.access(ProjectInputKind::Settings).unwrap(),
        ProjectAccess::Deferred { .. }
    ));
    service
        .set_session(WorkspaceTrustDecision::Restricted, 0)
        .unwrap();
    assert_eq!(
        service.access(ProjectInputKind::Instructions).unwrap(),
        ProjectAccess::Allowed
    );
    assert!(matches!(
        service.access(ProjectInputKind::Settings).unwrap(),
        ProjectAccess::Deferred { .. }
    ));
}

#[test]
fn noninteractive_frontends_never_prompt_or_trust_implicitly() {
    let root = tempfile::tempdir().unwrap();
    let interactive = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    assert!(matches!(
        interactive.prepare_startup(TrustFrontend::Interactive, None),
        Ok(TrustStartupState::Prompt(_))
    ));

    for frontend in [TrustFrontend::Headless, TrustFrontend::Acp] {
        let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
        assert!(matches!(
            service.prepare_startup(frontend, None),
            Err(WorkspaceTrustError::NonInteractiveTrustRequired { .. })
        ));
        assert_eq!(
            service.snapshot().unwrap().decision(),
            WorkspaceTrustDecision::Unknown
        );
    }

    let headless = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    let ready = headless
        .prepare_startup(
            TrustFrontend::Headless,
            Some(ExplicitWorkspaceTrust::TrustOnce),
        )
        .unwrap();
    let TrustStartupState::Ready(snapshot) = ready else {
        panic!("explicit noninteractive trust must be ready")
    };
    assert_eq!(snapshot.decision(), WorkspaceTrustDecision::Trusted);
    assert_eq!(snapshot.persistence(), TrustPersistence::Session);
}

#[test]
fn typed_dialog_actions_commit_exact_session_persistent_or_exit_outcomes() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    let dialog = service.dialog_state().unwrap();
    assert_eq!(
        dialog.actions(),
        [
            WorkspaceTrustAction::TrustOnce,
            WorkspaceTrustAction::TrustWorkspace,
            WorkspaceTrustAction::OpenRestricted,
            WorkspaceTrustAction::Exit,
        ]
    );
    assert!(matches!(
        service
            .apply_dialog_action(WorkspaceTrustAction::Exit, dialog.revision())
            .unwrap(),
        WorkspaceTrustActionOutcome::Exit
    ));
    assert_eq!(
        service.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
    let WorkspaceTrustActionOutcome::Ready(session) = service
        .apply_dialog_action(WorkspaceTrustAction::OpenRestricted, dialog.revision())
        .unwrap()
    else {
        panic!("restricted action must commit")
    };
    assert_eq!(session.decision(), WorkspaceTrustDecision::Restricted);
    assert_eq!(session.persistence(), TrustPersistence::Session);
    let WorkspaceTrustActionOutcome::Ready(persistent) = service
        .apply_dialog_action(WorkspaceTrustAction::TrustWorkspace, session.revision())
        .unwrap()
    else {
        panic!("persistent action must commit")
    };
    assert_eq!(persistent.decision(), WorkspaceTrustDecision::Trusted);
    assert_eq!(persistent.persistence(), TrustPersistence::Persistent);
    assert!(store.is_file());
}

#[test]
fn dialog_prompt_binds_actions_to_live_service_and_refreshes_stale_state() {
    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    let mut prompt = service.dialog_prompt().unwrap();
    assert_eq!(prompt.state().revision(), 0);
    assert!(format!("{prompt:?}").contains("<redacted>"));
    assert!(!format!("{prompt:?}").contains(root.path().to_string_lossy().as_ref()));

    service
        .set_session(WorkspaceTrustDecision::Trusted, 0)
        .unwrap();
    assert!(matches!(
        prompt.apply(WorkspaceTrustAction::OpenRestricted),
        Err(WorkspaceTrustError::Conflict { .. })
    ));
    let TrustStartupState::Ready(snapshot) = prompt.refresh().unwrap() else {
        panic!("authoritative committed state must be ready")
    };
    assert_eq!(snapshot.decision(), WorkspaceTrustDecision::Trusted);
    assert_eq!(snapshot.persistence(), TrustPersistence::Session);
}

#[test]
fn dialog_prompt_rejects_construction_after_trust_is_already_resolved() {
    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    service
        .set_session(WorkspaceTrustDecision::Restricted, 0)
        .unwrap();
    assert!(matches!(
        service.dialog_prompt(),
        Err(WorkspaceTrustError::InvalidDecision)
    ));
}

#[test]
fn malformed_or_symlinked_store_fails_without_guessing_trust() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(&store, "schema_version = 999\ngeneration = 0\n").unwrap();
    assert!(matches!(
        WorkspaceTrustService::file(&workspace, &store, strict()),
        Err(WorkspaceTrustError::InvalidStore)
    ));

    #[cfg(unix)]
    {
        let linked = root.path().join("linked.toml");
        std::os::unix::fs::symlink(&store, &linked).unwrap();
        assert!(matches!(
            WorkspaceTrustService::file(&workspace, &linked, strict()),
            Err(WorkspaceTrustError::InvalidStore)
        ));
    }
}

#[cfg(unix)]
#[test]
fn symlinked_store_parent_is_rejected_before_capability_open() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let actual = root.path().join("actual-home");
    let linked = root.path().join("linked-home");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &linked).unwrap();

    assert!(matches!(
        WorkspaceTrustService::file(&workspace, linked.join("trust.toml"), strict()),
        Err(WorkspaceTrustError::InvalidStore)
    ));
}

#[cfg(unix)]
#[test]
fn multiply_linked_store_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    service.persist(WorkspaceTrustDecision::Trusted, 0).unwrap();
    std::fs::hard_link(&store, root.path().join("home/trust-copy.toml")).unwrap();

    assert!(matches!(
        WorkspaceTrustService::file(&workspace, &store, strict()),
        Err(WorkspaceTrustError::InvalidStore)
    ));
}

#[cfg(unix)]
#[test]
fn reopening_repairs_owner_directory_and_file_modes_before_loading() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let store = home.join("trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    service.persist(WorkspaceTrustDecision::Trusted, 0).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o666)).unwrap();

    let reopened = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Trusted
    );
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(not(unix))]
#[test]
fn persistent_store_fails_closed_on_platforms_without_an_audited_owner_backend() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        WorkspaceTrustService::file(root.path(), root.path().join("trust.toml"), strict()),
        Err(WorkspaceTrustError::UnsupportedSecurity)
    ));
}

#[test]
fn second_store_owner_cannot_overwrite_a_newer_generation() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let first = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    let stale = WorkspaceTrustService::file(&workspace, &store, strict()).unwrap();
    first.persist(WorkspaceTrustDecision::Trusted, 0).unwrap();
    assert!(matches!(
        stale.persist(WorkspaceTrustDecision::Restricted, 0),
        Err(WorkspaceTrustError::StoreChanged)
    ));
    assert_eq!(
        stale.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
    assert_eq!(
        WorkspaceTrustService::file(&workspace, &store, strict())
            .unwrap()
            .snapshot()
            .unwrap()
            .decision(),
        WorkspaceTrustDecision::Trusted
    );
}

#[cfg(unix)]
#[test]
fn simultaneous_store_owners_admit_exactly_one_generation_cas() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    std::fs::create_dir(&workspace).unwrap();
    let services = (0..32)
        .map(|_| WorkspaceTrustService::file(&workspace, &store, strict()).unwrap())
        .collect::<Vec<_>>();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(services.len()));
    let handles = services
        .into_iter()
        .enumerate()
        .map(|(index, service)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                service.persist(
                    if index % 2 == 0 {
                        WorkspaceTrustDecision::Trusted
                    } else {
                        WorkspaceTrustDecision::Restricted
                    },
                    0,
                )
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "cross-instance generation CAS admitted more than one writer"
    );
    assert!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Err(WorkspaceTrustError::StoreChanged)))
            .count()
            >= 31
    );
}

#[cfg(unix)]
#[test]
fn cross_process_store_worker() {
    let Some(workspace) = std::env::var_os("HEYCODE_TRUST_TEST_WORKSPACE") else {
        return;
    };
    let store = std::env::var_os("HEYCODE_TRUST_TEST_STORE").unwrap();
    let ready = std::env::var_os("HEYCODE_TRUST_TEST_READY").unwrap();
    let go = std::env::var_os("HEYCODE_TRUST_TEST_GO").unwrap();
    let outcome = std::env::var_os("HEYCODE_TRUST_TEST_OUTCOME").unwrap();
    let decision = if std::env::var_os("HEYCODE_TRUST_TEST_DECISION").as_deref()
        == Some(std::ffi::OsStr::new("trusted"))
    {
        WorkspaceTrustDecision::Trusted
    } else {
        WorkspaceTrustDecision::Restricted
    };
    let service = WorkspaceTrustService::file(workspace, store, strict()).unwrap();
    std::fs::write(ready, b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::path::Path::new(&go).exists() {
        assert!(std::time::Instant::now() < deadline, "go barrier timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let label = match service.persist(decision, 0) {
        Ok(_) => "ok",
        Err(WorkspaceTrustError::StoreChanged) => "changed",
        Err(_) => "other",
    };
    std::fs::write(outcome, label).unwrap();
}

#[cfg(unix)]
#[test]
fn separate_processes_admit_exactly_one_generation_cas() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let store = root.path().join("home/trust.toml");
    let go = root.path().join("go");
    std::fs::create_dir(&workspace).unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    let mut ready = Vec::new();
    let mut outcomes = Vec::new();
    for index in 0..12 {
        let ready_path = root.path().join(format!("ready-{index}"));
        let outcome_path = root.path().join(format!("outcome-{index}"));
        let child = std::process::Command::new(&executable)
            .args(["--exact", "cross_process_store_worker", "--nocapture"])
            .env("HEYCODE_TRUST_TEST_WORKSPACE", &workspace)
            .env("HEYCODE_TRUST_TEST_STORE", &store)
            .env("HEYCODE_TRUST_TEST_READY", &ready_path)
            .env("HEYCODE_TRUST_TEST_GO", &go)
            .env("HEYCODE_TRUST_TEST_OUTCOME", &outcome_path)
            .env(
                "HEYCODE_TRUST_TEST_DECISION",
                if index % 2 == 0 {
                    "trusted"
                } else {
                    "restricted"
                },
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        children.push(child);
        ready.push(ready_path);
        outcomes.push(outcome_path);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while ready.iter().any(|path| !path.exists()) {
        assert!(
            std::time::Instant::now() < deadline,
            "worker readiness timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::write(&go, b"go").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let labels = outcomes
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        labels.iter().filter(|label| label.as_str() == "ok").count(),
        1
    );
    assert_eq!(
        labels
            .iter()
            .filter(|label| label.as_str() == "changed")
            .count(),
        11
    );
}

#[test]
fn ordinary_debug_and_errors_do_not_expose_workspace_or_store_paths() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("private-workspace-canary");
    std::fs::create_dir(&workspace).unwrap();
    let service = WorkspaceTrustService::memory(&workspace, strict()).unwrap();
    let snapshot = service.snapshot().unwrap();
    let identity_debug = format!("{:?}", snapshot.identity());
    assert!(!identity_debug.contains("private-workspace-canary"));
    assert!(identity_debug.contains("<redacted>"));
    let snapshot_debug = format!("{snapshot:?}");
    assert!(!snapshot_debug.contains("private-workspace-canary"));
    assert!(snapshot_debug.contains("<redacted>"));
    let dialog_debug = format!("{:?}", service.dialog_state().unwrap());
    assert!(!dialog_debug.contains("private-workspace-canary"));
    assert!(dialog_debug.contains("<redacted>"));

    let result = WorkspaceTrustService::file(
        &workspace,
        std::path::Path::new("relative-store-canary"),
        strict(),
    );
    let Err(error) = result else {
        panic!("relative store must fail")
    };
    let error = error.to_string();
    assert!(!error.contains("private-workspace-canary"));
    assert!(!error.contains("relative-store-canary"));
}

#[test]
fn plugin_publishes_exact_service_and_shutdown_invalidates_held_handle() {
    let root = tempfile::tempdir().unwrap();
    let service = WorkspaceTrustService::memory(root.path(), strict()).unwrap();
    let plugins = vec![heycode_trust::trust_service_plugin(service)];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let trust = context
        .get::<WorkspaceTrustService>(heycode_trust::SERVICE_TRUST)
        .unwrap();
    assert_eq!(
        trust.snapshot().unwrap().decision(),
        WorkspaceTrustDecision::Unknown
    );
    context.shutdown();
    assert!(matches!(
        trust.snapshot(),
        Err(WorkspaceTrustError::ServiceUnavailable)
    ));
}
