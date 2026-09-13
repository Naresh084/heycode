//! Focused source-scoped `/memory` manager verification.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use heycode_prompt::instructions::InstructionSources;
use heycode_tui::memory_commands::{
    MemoryAuthority, MemoryManagerError, MemorySourceKind, MemorySourceManager, MemorySourceScope,
    MemorySourceStatus, register_memory_command,
};
use sha2::{Digest as _, Sha256};

#[derive(Default)]
struct FixedAuthority {
    sources: Mutex<InstructionSources>,
}

impl FixedAuthority {
    fn new(user_home: Option<PathBuf>, workspace: Option<PathBuf>) -> Self {
        Self {
            sources: Mutex::new(InstructionSources {
                user_home,
                workspace,
            }),
        }
    }

    fn set_workspace(&self, workspace: Option<PathBuf>) {
        self.sources.lock().unwrap().workspace = workspace;
    }
}

impl MemoryAuthority for FixedAuthority {
    fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
        Ok(self.sources.lock().unwrap().clone())
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn inventory_attributes_fixed_and_auto_memory_sources_without_absolute_labels() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write(&home.path().join("AGENTS.md"), "user law");
    write(
        &home.path().join(".agent-memory/user/reviewer/MEMORY.md"),
        "user notes",
    );
    write(
        &project
            .path()
            .join(".heycode/agent-memory/builder/MEMORY.md"),
        "project notes",
    );
    let canonical_project = project.path().canonicalize().unwrap();
    let local_key = format!(
        "{:x}",
        Sha256::digest(canonical_project.to_string_lossy().as_bytes())
    );
    write(
        &home
            .path()
            .join(".agent-memory/local")
            .join(local_key)
            .join("advisor/MEMORY.md"),
        "local notes",
    );
    let manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    )));
    let snapshot = manager.snapshot().unwrap();
    let ids = snapshot
        .sources
        .iter()
        .map(|source| source.id())
        .collect::<Vec<_>>();
    assert_eq!(
        &ids[..6],
        [
            "user:agents",
            "project:agents",
            "project:claude",
            "project:heycode",
            "project:agents-local",
            "project:claude-local",
        ]
    );
    assert!(ids.contains(&"auto:user:reviewer"));
    assert!(ids.contains(&"auto:project:builder"));
    assert!(ids.contains(&"auto:local:advisor"));
    let user = snapshot
        .sources
        .iter()
        .find(|source| source.id() == "user:agents")
        .unwrap();
    assert_eq!(user.label(), "~/.heycode/AGENTS.md");
    assert_eq!(user.scope(), MemorySourceScope::User);
    assert_eq!(user.kind(), MemorySourceKind::Instructions);
    assert!(matches!(user.status(), MemorySourceStatus::Ready { .. }));
    assert!(snapshot.warnings.is_empty());
}

#[test]
fn auto_memory_uses_the_subagent_state_root_not_the_instruction_home() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write(
        &home.path().join(".agent-memory/user/wrong/MEMORY.md"),
        "not the runtime store",
    );
    write(
        &state.path().join(".agent-memory/user/right/MEMORY.md"),
        "runtime notes",
    );
    let manager = MemorySourceManager::with_auto_memory_root(
        Arc::new(FixedAuthority::new(Some(home.path().to_path_buf()), None)),
        Some(state.path().to_path_buf()),
    );
    let snapshot = manager.snapshot().unwrap();
    assert!(
        snapshot
            .sources
            .iter()
            .any(|source| source.id() == "auto:user:right")
    );
    assert!(
        snapshot
            .sources
            .iter()
            .all(|source| source.id() != "auto:user:wrong")
    );
}

#[test]
fn revision_checked_writes_stale_safely_and_report_only_real_prompt_changes() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write(
        &home.path().join(".agent-memory/user/reviewer/MEMORY.md"),
        "old notes",
    );
    let manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    )));
    let missing = manager.read("project:agents").unwrap();
    let created = manager
        .replace("project:agents", &missing.revision, "project law")
        .unwrap();
    assert!(created.created && created.prompt_changed);
    assert_eq!(
        manager
            .replace("project:agents", &missing.revision, "stale")
            .unwrap_err(),
        MemoryManagerError::StaleRevision
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("AGENTS.md")).unwrap(),
        "project law"
    );
    let auto = manager.read("auto:user:reviewer").unwrap();
    let updated = manager
        .replace("auto:user:reviewer", &auto.revision, "new notes")
        .unwrap();
    assert!(!updated.created && !updated.prompt_changed);
}

#[test]
fn changing_or_removing_workspace_trust_cannot_apply_an_old_project_edit() {
    let home = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let authority = Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        Some(first.path().to_path_buf()),
    ));
    let manager = MemorySourceManager::new(authority.clone());
    let old = manager.read("project:agents").unwrap();
    authority.set_workspace(Some(second.path().to_path_buf()));
    assert_eq!(
        manager
            .replace("project:agents", &old.revision, "wrong project")
            .unwrap_err(),
        MemoryManagerError::StaleRevision
    );
    assert!(!second.path().join("AGENTS.md").exists());
    authority.set_workspace(None);
    assert_eq!(
        manager.read("project:agents").unwrap_err(),
        MemoryManagerError::UnknownSource
    );
    assert!(
        manager
            .snapshot()
            .unwrap()
            .sources
            .iter()
            .all(|source| source.scope() == MemorySourceScope::User)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_instruction_and_auto_memory_sources_are_blocked_or_skipped() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("MEMORY.md"), "outside secret");
    std::fs::create_dir_all(home.path().join(".agent-memory/user")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        home.path().join(".agent-memory/user/reviewer"),
    )
    .unwrap();
    write(&outside.path().join("AGENTS.md"), "outside instruction");
    std::os::unix::fs::symlink(
        outside.path().join("AGENTS.md"),
        project.path().join("AGENTS.md"),
    )
    .unwrap();
    let manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        Some(project.path().to_path_buf()),
    )));
    let snapshot = manager.snapshot().unwrap();
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("auto:user:reviewer") && warning.contains("unsafe"))
    );
    assert_eq!(
        manager.read("project:agents").unwrap_err(),
        MemoryManagerError::UnsafeSource
    );
    assert_eq!(
        manager.read("auto:user:reviewer").unwrap_err(),
        MemoryManagerError::UnknownSource
    );

    let linked_home = tempfile::tempdir().unwrap();
    let linked_target = tempfile::tempdir().unwrap();
    write(
        &linked_target.path().join("user/private-name/MEMORY.md"),
        "outside secret",
    );
    std::os::unix::fs::symlink(
        linked_target.path(),
        linked_home.path().join(".agent-memory"),
    )
    .unwrap();
    let linked_manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(linked_home.path().to_path_buf()),
        None,
    )));
    let linked_snapshot = linked_manager.snapshot().unwrap();
    assert!(
        linked_snapshot
            .warnings
            .iter()
            .any(|warning| warning == "auto:user: auto-memory directory is unsafe")
    );
    assert!(
        linked_snapshot
            .sources
            .iter()
            .all(|source| source.id() != "auto:user:private-name")
    );

    let hardlink_home = tempfile::tempdir().unwrap();
    let hardlink_project = tempfile::tempdir().unwrap();
    let linked_file = hardlink_home.path().join("outside.md");
    std::fs::write(&linked_file, "shared secret").unwrap();
    std::fs::hard_link(&linked_file, hardlink_project.path().join("AGENTS.md")).unwrap();
    let hardlink_manager = MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(hardlink_home.path().to_path_buf()),
        Some(hardlink_project.path().to_path_buf()),
    )));
    assert_eq!(
        hardlink_manager.read("project:agents").unwrap_err(),
        MemoryManagerError::UnsafeSource
    );
}

#[test]
fn command_registration_is_canonical_queued_and_source_attributed() {
    let home = tempfile::tempdir().unwrap();
    let manager = Arc::new(MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        None,
    ))));
    let context = heycode_core::compose(&[heycode_agent::commands_plugin()]).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    register_memory_command(&context, &commands, manager).unwrap();
    let command = commands.get("memory").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "memory-commands");
    assert_eq!(
        command.descriptor().timing(),
        heycode_agent::CommandTiming::Queued
    );
    assert_eq!(
        command.descriptor().synopsis(),
        "/memory [action] [arguments...]"
    );
    assert!(command.availability().is_available());
}

#[tokio::test]
async fn slash_command_executes_manager_without_a_model_request_or_session_message() {
    use heycode_core::Plugin;
    use heycode_llm::testing::FakeProvider;
    use heycode_llm::{LlmSelection, Provider, llm_plugin};

    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let home = tempfile::tempdir().unwrap();
    write(&home.path().join("AGENTS.md"), "user law");
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(Vec::new()));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(cwd.clone()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                cwd.clone(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_hooks::hooks_plugin(heycode_trust::WorkspaceTrustDecision::Trusted),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig {
            web_enabled: false,
            ..heycode_tools::ToolsConfig::default()
        }),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".to_owned(),
                model: "memory-test".to_owned(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::DenyAll)),
        heycode_agent::agent_options_plugin(heycode_agent::AgentOptions {
            cwd: Some(cwd),
            ..heycode_agent::AgentOptions::default()
        }),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::agent_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let manager = Arc::new(MemorySourceManager::new(Arc::new(FixedAuthority::new(
        Some(home.path().to_path_buf()),
        None,
    ))));
    let revision = manager.read("user:agents").unwrap().revision;
    register_memory_command(&context, &commands, manager).unwrap();
    let output = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = output.clone();
    context
        .events
        .on::<heycode_agent::UiEvent>(move |event| match event {
            heycode_agent::UiEvent::Info { text } => sink.lock().unwrap().push(text.clone()),
            heycode_agent::UiEvent::CapabilityPanelRequested { panel } => {
                sink.lock().unwrap().push(format!("panel:{panel}"));
            }
            _ => {}
        });
    let before = agent.session().lock().unwrap().events().len();
    let command = commands.get("memory").unwrap().unwrap();
    command.execute(&agent, "").await.unwrap();
    assert_eq!(&*output.lock().unwrap(), &["panel:memory"]);
    output.lock().unwrap().clear();
    command.execute(&agent, "list").await.unwrap();
    let rendered = output.lock().unwrap().join("\n");
    assert!(rendered.contains("user:agents"), "{rendered}");
    assert!(
        rendered.contains("rendered fresh on the next request"),
        "{rendered}"
    );
    assert!(!rendered.contains(&revision), "{rendered}");
    assert!(rendered.contains("/memory show user:agents for details"));
    assert_eq!(agent.session().lock().unwrap().events().len(), before);
}
