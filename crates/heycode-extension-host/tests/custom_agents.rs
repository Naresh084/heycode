//! File authoring, precedence, strict imports and live registry replacement.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use heycode_extension_host::user_declarations::{
    UserDeclarationRoots, WorkspaceDeclarationAccess, load_user_declarations,
    validate_agent_document,
};
use heycode_extension_host::{AgentDeclarationService, AgentImportFormat, import_agent};
use std::sync::Arc;

fn document(label: &str) -> String {
    serde_json::json!({"display":label,"instructions":format!("{label} instructions"),"config":{"permissions":"read_only","max_turns":4}}).to_string()
}
fn write(root: &std::path::Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn project_alias_precedence_reload_deletion_and_invalid_generation_are_exact() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write(home.path(), "agents/reviewer.json", &document("User"));
    write(
        project.path(),
        ".heycode/agents/reviewer.json",
        &document("Project"),
    );
    let roots = UserDeclarationRoots {
        user_home: Some(home.path().to_path_buf()),
        workspace: Some(WorkspaceDeclarationAccess {
            root: project.path().to_path_buf(),
            executables_allowed: false,
        }),
    };
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let service = AgentDeclarationService::new(roots.clone(), registry.clone());
    service.reload().unwrap();
    assert_eq!(registry.preset("reviewer").unwrap().display(), "Project");
    assert_eq!(registry.preset("user-reviewer").unwrap().display(), "User");
    assert_eq!(
        registry.preset("project-reviewer").unwrap().display(),
        "Project"
    );
    let resolved = registry.preset("reviewer").unwrap();
    write(
        project.path(),
        ".heycode/agents/reviewer.json",
        &document("Updated"),
    );
    service.reload().unwrap();
    assert_eq!(registry.preset("reviewer").unwrap().display(), "Updated");
    assert_eq!(
        resolved.display(),
        "Project",
        "already-resolved child snapshots are immutable"
    );
    write(
        project.path(),
        ".heycode/agents/reviewer.json",
        r#"{"display":"bad","instructions":"bad","config":{"permissions":"bypass"}}"#,
    );
    let error = service.reload().unwrap_err();
    assert!(error.contains("permissions") || error.contains("bypass"));
    assert_eq!(registry.preset("reviewer").unwrap().display(), "Updated");
    std::fs::remove_file(project.path().join(".heycode/agents/reviewer.json")).unwrap();
    service.reload().unwrap();
    assert_eq!(registry.preset("reviewer").unwrap().display(), "User");
    assert!(registry.preset("project-reviewer").is_none());
    service.close();
    assert!(registry.presets().is_empty());
    assert!(service.reload().is_err());
    assert!(service.save("new", &document("New"), false).is_err());
    let restricted = load_user_declarations(&UserDeclarationRoots {
        user_home: roots.user_home,
        workspace: None,
    });
    assert_eq!(restricted.presets.len(), 2);
}

#[test]
fn authoring_validates_before_write_and_never_clobbers_on_create() {
    let home = tempfile::tempdir().unwrap();
    let service = AgentDeclarationService::new(
        UserDeclarationRoots {
            user_home: Some(home.path().to_path_buf()),
            workspace: None,
        },
        Arc::new(heycode_agent::SubagentRegistry::new()),
    );
    let path = service
        .save("new-agent", &document("First"), false)
        .unwrap();
    assert!(
        service
            .save("new-agent", &document("Second"), false)
            .is_err()
    );
    assert!(service.save("../escape", &document("Bad"), true).is_err());
    assert!(
        service
            .save(
                "new-agent",
                r#"{"display":"bad","instructions":"bad","config":{"max_turns":0}}"#,
                true
            )
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), document("First"));
    service
        .save("new-agent", &document("Second"), true)
        .unwrap();
    service.reload().unwrap();
    assert!(
        validate_agent_document(
            r#"{"display":"x","instructions":"x","config":{"unsupported":true}}"#
        )
        .unwrap_err()
        .contains("unsupported")
    );
    assert!(
        validate_agent_document(
            r#"{"display":"x","instructions":"x","config":{"tools":["read","read"]}}"#
        )
        .is_err()
    );
    assert!(
        validate_agent_document(
            r#"{"display":"x","instructions":"x","config":{"inference_provider":"openai"}}"#
        )
        .is_err()
    );
}

#[test]
fn foreign_registration_conflict_keeps_entire_previous_generation() {
    let home = tempfile::tempdir().unwrap();
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let service = AgentDeclarationService::new(
        UserDeclarationRoots {
            user_home: Some(home.path().to_path_buf()),
            workspace: None,
        },
        registry.clone(),
    );
    service.save("first", &document("First"), false).unwrap();
    service.reload().unwrap();
    let foreign = registry
        .register_preset_owned(
            heycode_agent::SubagentPreset::new(
                "second",
                "Foreign",
                "Foreign instructions",
                None,
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
            )
            .unwrap(),
        )
        .unwrap();
    service.save("second", &document("Second"), false).unwrap();
    assert!(service.reload().unwrap_err().contains("conflicts"));
    assert_eq!(registry.preset("first").unwrap().display(), "First");
    assert_eq!(registry.preset("second").unwrap().display(), "Foreign");
    assert!(registry.preset("user-second").is_none());
    service.close();
    assert!(registry.preset("first").is_none());
    assert!(registry.preset("second").is_some());
    drop(foreign);
}

#[test]
fn claude_and_codex_import_preserve_supported_controls_and_refuse_unknown_semantics() {
    let claude = "---\nname: code-reviewer\ndescription: Review code\nmodel: inherit\ntools: [Read, Grep]\ndisallowedTools: Bash\npermissionMode: plan\nmaxTurns: 8\neffort: high\nmemory: local\nmcpServers: [docs]\nbackground: true\n---\nReview carefully.\n";
    let imported = import_agent(claude, AgentImportFormat::Claude).unwrap();
    let json: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(json["instructions"], "Review carefully.");
    assert_eq!(json["description"], "Review code");
    assert_eq!(json["config"]["tools"], serde_json::json!(["read", "grep"]));
    assert_eq!(json["config"]["denied_tools"], serde_json::json!(["bash"]));
    assert_eq!(json["config"]["permissions"], "read_only");
    assert_eq!(json["config"]["max_turns"], 8);
    assert_eq!(json["config"]["memory"], "local");
    assert_eq!(json["config"]["mcp_servers"], serde_json::json!(["docs"]));
    let codex = "name='reviewer'\ndescription='Reviews'\nmodel='model-1'\nmodel_reasoning_effort='high'\nsandbox_mode='read-only'\ndeveloper_instructions='Review precisely.'";
    let imported = import_agent(codex, AgentImportFormat::Codex).unwrap();
    let json: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(json["config"]["model"], "model-1");
    assert_eq!(json["config"]["effort"], "high");
    assert_eq!(json["instructions"], "Review precisely.");
    for (source, format, field) in [
        (
            format!("{codex}\n[network]\nenabled=true"),
            AgentImportFormat::Codex,
            "network",
        ),
        (
            claude.replace("name: code-reviewer", "name: code-reviewer\nhooks: {}"),
            AgentImportFormat::Claude,
            "hooks",
        ),
        (
            claude.replace(
                "name: code-reviewer",
                "name: code-reviewer\nskills: [special]",
            ),
            AgentImportFormat::Claude,
            "skills",
        ),
    ] {
        assert!(import_agent(&source, format).unwrap_err().contains(field));
    }
    assert!(
        import_agent(
            &codex.replace("read-only", "workspace-write"),
            AgentImportFormat::Codex
        )
        .is_err()
    );
    assert!(
        import_agent(
            &claude.replace("model: inherit", "model: sonnet"),
            AgentImportFormat::Claude
        )
        .unwrap_err()
        .contains("aliases")
    );
    assert!(
        import_agent(
            &claude.replace("permissionMode: plan", "permissionMode: bypassPermissions"),
            AgentImportFormat::Claude
        )
        .is_err()
    );
    assert!(
        import_agent(
            &claude.replace("[Read, Grep]", "[Bash(git status)]"),
            AgentImportFormat::Claude
        )
        .is_err()
    );
    assert!(import_agent("---\nname: missing-body", AgentImportFormat::Claude).is_err());
    assert!(import_agent("name = [", AgentImportFormat::Codex).is_err());
}

#[cfg(unix)]
#[test]
fn unreadable_shapes_symlinks_and_oversized_files_are_visible_and_block_reload() {
    let home = tempfile::tempdir().unwrap();
    let external = tempfile::NamedTempFile::new().unwrap();
    let roots = UserDeclarationRoots {
        user_home: Some(home.path().to_path_buf()),
        workspace: None,
    };
    write(home.path(), "agents/good.json", &document("Good"));
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let service = AgentDeclarationService::new(roots.clone(), registry.clone());
    service.reload().unwrap();
    std::os::unix::fs::symlink(external.path(), home.path().join("agents/symlink.json")).unwrap();
    std::fs::write(
        home.path().join("agents/large.json"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    let loaded = load_user_declarations(&roots);
    assert_eq!(loaded.skipped.len(), 2);
    assert!(service.reload().is_err());
    assert_eq!(registry.preset("good").unwrap().display(), "Good");
    assert!(service.save("symlink", &document("Unsafe"), true).is_err());
    assert_eq!(std::fs::read(external.path()).unwrap(), Vec::<u8>::new());
}

#[test]
fn legacy_scope_prefixed_file_names_remain_available_without_alias_collisions() {
    let home = tempfile::tempdir().unwrap();
    write(home.path(), "agents/reviewer.json", &document("Regular"));
    write(
        home.path(),
        "agents/user-reviewer.json",
        &document("Legacy prefixed"),
    );
    let registry = Arc::new(heycode_agent::SubagentRegistry::new());
    let service = AgentDeclarationService::new(
        UserDeclarationRoots {
            user_home: Some(home.path().to_path_buf()),
            workspace: None,
        },
        registry.clone(),
    );
    service.reload().unwrap();
    assert_eq!(registry.preset("reviewer").unwrap().display(), "Regular");
    assert_eq!(
        registry.preset("user-reviewer").unwrap().display(),
        "Regular"
    );
    assert_eq!(
        registry.preset("user-user-reviewer").unwrap().display(),
        "Legacy prefixed"
    );
}
