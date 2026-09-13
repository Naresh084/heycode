//! CMD05 preview, preservation, stale-state and plugin-lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_init::{
    InitApplyOutcome, InitChangeKind, InitError, InitService, MANAGED_END, MANAGED_START,
    init_plugin,
};

#[test]
fn new_rust_workspace_previews_then_atomically_creates_useful_instructions() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    let service = InitService::new(root.path()).unwrap();

    let preview = service.preview().unwrap();
    assert_eq!(preview.change(), InitChangeKind::Create);
    assert!(!root.path().join("AGENTS.md").exists());
    let rendered = preview.render();
    assert!(rendered.contains("No file changed"), "{rendered}");
    assert!(rendered.contains("cargo fmt --all --check"), "{rendered}");
    assert!(rendered.contains("/init apply "), "{rendered}");

    let outcome = service.apply(preview.token()).unwrap();
    assert_eq!(outcome, InitApplyOutcome::Created);
    let written = std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap();
    assert!(written.starts_with("# AGENTS.md\n\n"), "{written}");
    assert!(written.contains(MANAGED_START));
    assert!(written.contains(MANAGED_END));
    assert!(written.contains("cargo clippy --workspace --all-targets -- -D warnings"));
    assert!(written.ends_with('\n'));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(root.path().join("AGENTS.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    let current = service.preview().unwrap();
    assert_eq!(current.change(), InitChangeKind::Unchanged);
    assert!(!current.render().contains("/init apply "));
}

#[test]
fn existing_project_law_is_byte_preserved_while_only_the_managed_section_refreshes() {
    let root = tempfile::tempdir().unwrap();
    let agents = root.path().join("AGENTS.md");
    let project_law = "# Project law\n\nNever delete this exact text.\n";
    std::fs::write(&agents, project_law).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    let service = InitService::new(root.path()).unwrap();

    let append = service.preview().unwrap();
    assert_eq!(append.change(), InitChangeKind::Append);
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), project_law);
    assert_eq!(
        service.apply(append.token()).unwrap(),
        InitApplyOutcome::Updated
    );
    let first = std::fs::read_to_string(&agents).unwrap();
    assert!(first.starts_with(project_law));
    assert_eq!(first.matches(MANAGED_START).count(), 1);
    assert_eq!(first.matches(MANAGED_END).count(), 1);

    std::fs::write(
        root.path().join("go.mod"),
        "module example.test/workspace\n",
    )
    .unwrap();
    let refresh = service.preview().unwrap();
    assert_eq!(refresh.change(), InitChangeKind::Refresh);
    assert!(refresh.render().contains("go test ./..."));
    assert_eq!(
        service.apply(refresh.token()).unwrap(),
        InitApplyOutcome::Updated
    );
    let second = std::fs::read_to_string(&agents).unwrap();
    assert!(second.starts_with(project_law));
    assert!(second.contains("go test ./..."));
    assert_eq!(second.matches(MANAGED_START).count(), 1);
    assert_eq!(second.matches(MANAGED_END).count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn stale_preview_refuses_to_overwrite_a_concurrent_user_edit() {
    let root = tempfile::tempdir().unwrap();
    let service = InitService::new(root.path()).unwrap();
    let preview = service.preview().unwrap();
    let teammate_edit = "# Teammate instructions\n\nKeep this.\n";
    std::fs::write(root.path().join("AGENTS.md"), teammate_edit).unwrap();

    let error = service.apply(preview.token()).unwrap_err();
    assert!(matches!(error, InitError::StalePreview));
    assert_eq!(
        std::fs::read_to_string(root.path().join("AGENTS.md")).unwrap(),
        teammate_edit
    );
}

#[test]
fn unsafe_targets_and_malformed_managed_markers_fail_without_mutation() {
    let malformed_root = tempfile::tempdir().unwrap();
    let malformed = format!("# Law\n\n{MANAGED_START}\nunfinished\n");
    std::fs::write(malformed_root.path().join("AGENTS.md"), &malformed).unwrap();
    let service = InitService::new(malformed_root.path()).unwrap();
    assert!(matches!(
        service.preview().unwrap_err(),
        InitError::MalformedManagedSection
    ));
    assert_eq!(
        std::fs::read_to_string(malformed_root.path().join("AGENTS.md")).unwrap(),
        malformed
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let linked_root = tempfile::tempdir().unwrap();
        let outside = linked_root.path().join("outside.md");
        std::fs::write(&outside, "outside\n").unwrap();
        symlink(&outside, linked_root.path().join("AGENTS.md")).unwrap();
        let linked = InitService::new(linked_root.path()).unwrap();
        assert!(matches!(
            linked.preview().unwrap_err(),
            InitError::UnsafeTarget
        ));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside\n");
    }
}

#[test]
fn command_is_preview_first_and_its_plugin_registration_disposes() {
    let root = tempfile::tempdir().unwrap();
    let service = InitService::new(root.path()).unwrap();
    let preview_message = service.execute("").unwrap();
    assert!(preview_message.contains("No file changed"));
    assert!(!root.path().join("AGENTS.md").exists());
    assert!(matches!(service.execute("apply"), Err(InitError::Usage)));
    assert!(matches!(
        service.execute("overwrite"),
        Err(InitError::Usage)
    ));
    let preview = service.preview().unwrap();
    let applied = service
        .execute(&format!("apply {}", preview.token().as_str()))
        .unwrap();
    assert!(applied.contains("Created AGENTS.md"), "{applied}");

    let plugin_root = tempfile::tempdir().unwrap();
    let plugins = vec![
        heycode_agent::commands_plugin(),
        init_plugin(plugin_root.path().to_path_buf()),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let command = commands.get("init").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "init");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Queued
    );
    assert_eq!(command.descriptor().synopsis(), "/init [action] [token]");
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "init"
                && row.kind == heycode_core::ContributionKind::Command
                && row.name == "init")
    );

    context.shutdown();
    assert!(commands.get("init").unwrap().is_none());
}
