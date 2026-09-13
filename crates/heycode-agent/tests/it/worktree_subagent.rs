#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::sync::Arc;

use heycode_agent::{GitCommitId, GitWorktreeManager, WorktreeOutcome, WorktreeRetention};
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
    assert!(output.status.success(), "git command failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository(root: &std::path::Path) -> (std::path::PathBuf, GitCommitId) {
    let repository = root.join("repository");
    std::fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "-q"]);
    std::fs::write(repository.join("tracked.txt"), "base\n").unwrap();
    git(&repository, &["add", "--", "tracked.txt"]);
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
    let base = GitCommitId::new(git(&repository, &["rev-parse", "HEAD"])).unwrap();
    (repository, base)
}

#[tokio::test]
async fn exact_base_create_and_success_cleanup_use_managed_worktree_only() {
    let root = tempfile::tempdir().unwrap();
    let (repository, base) = repository(root.path());
    let managed = root.path().join("managed;touch-pwned");
    let manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository,
            managed.clone(),
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let expected_managed = std::fs::canonicalize(managed.parent().unwrap())
        .unwrap()
        .join(managed.file_name().unwrap());
    assert_eq!(
        manager
            .recover(CancellationToken::new())
            .await
            .unwrap()
            .removed(),
        0
    );
    let lease = manager
        .create(base.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert!(lease.path().starts_with(&expected_managed));
    assert_eq!(git(lease.path(), &["rev-parse", "HEAD"]), base.as_str());
    assert_eq!(
        std::fs::read_to_string(lease.path().join("tracked.txt")).unwrap(),
        "base\n"
    );
    let path = lease.path().to_path_buf();
    lease
        .finish(WorktreeOutcome::Success, CancellationToken::new())
        .await
        .unwrap();
    assert!(!path.exists());
    assert!(!root.path().join("touch-pwned").exists());
}

#[tokio::test]
async fn dropped_lease_is_recovered_but_explicit_failure_retention_is_not() {
    let root = tempfile::tempdir().unwrap();
    let (repository, base) = repository(root.path());
    let managed = root.path().join("managed");
    let manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository.clone(),
            managed.clone(),
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let stale = manager
        .create(base.clone(), CancellationToken::new())
        .await
        .unwrap();
    let stale_path = stale.path().to_path_buf();
    drop(stale);
    drop(manager);

    let recovered = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository.clone(),
            managed.clone(),
            WorktreeRetention::RetainOnFailure,
        )
        .unwrap(),
    );
    let report = recovered.recover(CancellationToken::new()).await.unwrap();
    assert_eq!(report.removed(), 1);
    assert!(!stale_path.exists());

    let retained = recovered
        .create(base, CancellationToken::new())
        .await
        .unwrap();
    let retained_id = retained.id().clone();
    let retained_path = retained.path().to_path_buf();
    retained
        .finish(WorktreeOutcome::Failure, CancellationToken::new())
        .await
        .unwrap();
    assert!(retained_path.exists());
    drop(recovered);

    let final_manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository,
            managed,
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let report = final_manager
        .recover(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.retained(), 1);
    assert!(retained_path.exists());
    final_manager
        .cleanup_retained(&retained_id, CancellationToken::new())
        .await
        .unwrap();
    assert!(!retained_path.exists());
}

#[tokio::test]
async fn every_terminal_outcome_and_crash_preserve_tracked_untracked_and_committed_results() {
    for outcome in [
        Some(WorktreeOutcome::Success),
        Some(WorktreeOutcome::Cancelled),
        Some(WorktreeOutcome::Failure),
        None,
    ] {
        let root = tempfile::tempdir().unwrap();
        let (repository, base) = repository(root.path());
        let manager = Arc::new(
            GitWorktreeManager::new(
                heycode_exec::SubprocessService::local(),
                repository.clone(),
                root.path().join("managed"),
                WorktreeRetention::RemoveAlways,
            )
            .unwrap(),
        );
        let lease = manager
            .create(base, CancellationToken::new())
            .await
            .unwrap();
        let id = lease.id().clone();
        let path = lease.path().to_path_buf();
        std::fs::write(path.join("tracked.txt"), "changed\n").unwrap();
        std::fs::write(path.join("new.bin"), [0, 255, 1, 2]).unwrap();
        if let Some(outcome) = outcome {
            lease
                .finish(outcome, CancellationToken::new())
                .await
                .unwrap();
        } else {
            drop(lease);
        }
        drop(manager);
        let recovery = Arc::new(
            GitWorktreeManager::new(
                heycode_exec::SubprocessService::local(),
                repository,
                root.path().join("managed"),
                WorktreeRetention::RemoveAlways,
            )
            .unwrap(),
        );
        assert_eq!(
            recovery
                .recover(CancellationToken::new())
                .await
                .unwrap()
                .retained(),
            1
        );
        assert_eq!(
            std::fs::read(path.join("tracked.txt")).unwrap(),
            b"changed\n"
        );
        assert_eq!(std::fs::read(path.join("new.bin")).unwrap(), [0, 255, 1, 2]);
        recovery
            .cleanup_retained(&id, CancellationToken::new())
            .await
            .unwrap();
        assert!(!path.exists());
    }
    let root = tempfile::tempdir().unwrap();
    let (repository, base) = repository(root.path());
    let manager = Arc::new(
        GitWorktreeManager::new(
            heycode_exec::SubprocessService::local(),
            repository,
            root.path().join("managed"),
            WorktreeRetention::RemoveAlways,
        )
        .unwrap(),
    );
    let lease = manager
        .create(base, CancellationToken::new())
        .await
        .unwrap();
    let path = lease.path().to_path_buf();
    std::fs::write(path.join("tracked.txt"), "committed\n").unwrap();
    git(&path, &["add", "."]);
    git(
        &path,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "child result",
        ],
    );
    lease
        .finish(WorktreeOutcome::Success, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(path.join("tracked.txt")).unwrap(),
        b"committed\n"
    );
}
