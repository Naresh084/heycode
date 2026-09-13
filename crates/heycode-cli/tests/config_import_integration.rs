//! Real production composition, human confirmation and native resource activation.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use heycode_agent::{Agent, CommandRegistry, UiEvent};
use heycode_cli::config_import_integration::ConfigImportHandle;
use heycode_cli::testing::RealCompositionHarness;
use heycode_config::imports::{
    ImportCommitOutcome, ImportError, ImportProduct, ImportStore, ImportTarget,
};
use heycode_extension_host::config_import::{
    ConfigImportDecision, ConfigImportRequest, ImportItemStatus, SERVICE_CONFIG_IMPORT,
};
use heycode_llm::{ChatRequest, FinishReason, Provider, ProviderInfo, StreamChunk};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Capture(Mutex<Vec<ChatRequest>>);
#[async_trait::async_trait]
impl Provider for Capture {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "fake".into(),
            default_model: "fake-model".into(),
        }
    }
    fn stream(&self, request: ChatRequest) -> heycode_llm::ChunkStream {
        self.0.lock().unwrap().push(request);
        Box::pin(futures::stream::iter(vec![
            Ok(StreamChunk::TextDelta("done".into())),
            Ok(StreamChunk::Finish(FinishReason::Stop)),
        ]))
    }
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn imports(
    context: &heycode_core::Context,
) -> Arc<heycode_extension_host::config_import::ConfigImportService> {
    context
        .get::<ConfigImportHandle>(SERVICE_CONFIG_IMPORT)
        .expect("production import owner")
        .0
        .clone()
}
fn capture_info(agent: &Agent) -> Arc<Mutex<Vec<String>>> {
    let output = Arc::new(Mutex::new(Vec::new()));
    let captured = output.clone();
    agent.ui().on::<UiEvent>(move |event| {
        if let UiEvent::Info { text } = event {
            captured.lock().unwrap().push(text.clone());
        }
    });
    output
}
async fn command(context: &heycode_core::Context, args: &str) {
    let agent = context.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    context
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("import")
        .unwrap()
        .unwrap()
        .execute(&agent, args)
        .await
        .unwrap();
}
async fn import_ready(
    context: &heycode_core::Context,
    output: &Mutex<Vec<String>>,
    product: &str,
    root: &Path,
) {
    command(context, &format!("{product} {}", root.display())).await;
    let scan = output.lock().unwrap().last().unwrap().clone();
    let ids = scan
        .lines()
        .filter(|line| line.contains(" | Ready | "))
        .map(|line| line.split(" | ").next().unwrap())
        .collect::<Vec<_>>()
        .join(",");
    assert!(!ids.is_empty(), "{scan}");
    command(context, &format!("select {ids}")).await;
    let preview = output.lock().unwrap().last().unwrap().clone();
    let digest = preview
        .split("/import confirm ")
        .nth(1)
        .expect("exact confirmation action")
        .trim();
    command(context, &format!("confirm {digest}")).await;
    let committed = output.lock().unwrap().last().unwrap().clone();
    assert!(committed.starts_with("Imported "), "{committed}");
}

fn recompose(
    root: &Path,
    config: &heycode_config::Config,
    provider: Arc<dyn Provider>,
) -> heycode_core::Context {
    let trust = heycode_trust::WorkspaceTrustService::memory(
        root.join("workspace"),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    trust
        .set_session(
            heycode_trust::WorkspaceTrustDecision::Trusted,
            trust.snapshot().unwrap().revision(),
        )
        .unwrap();
    heycode_cli::compose_world(&heycode_cli::WorldOptions {
        config,
        trust,
        config_migration: None,
        profile_layers: &[],
        sessions_dir: root.join("sessions"),
        attachments_dir: root.join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: root.join("settings.toml"),
        credentials_root: root.join("credentials-home"),
        catalog_cache_path: root.join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: root.join("workspace"),
        fake: Some(provider),
        resume: None,
    })
    .unwrap()
}

#[tokio::test]
async fn human_import_commits_one_generation_then_recomposition_uses_frozen_native_resources() {
    let mut harness = RealCompositionHarness::new()
        .unwrap()
        .with_trusted_workspace();
    let root = std::fs::canonicalize(harness.root()).unwrap();
    let native_home = harness.credentials_root();
    let config = harness.config_mut().clone();
    write(&native_home, "AGENTS.md", "NATIVE_USER_GUIDANCE");
    write(&root, "workspace/AGENTS.md", "NATIVE_PROJECT_GUIDANCE");
    write(&root, "workspace/GEMINI.md", "IMPORTED_PROJECT_GUIDANCE");
    write(
        &root,
        "settings.toml",
        "# native settings must stay byte-identical\nschema_version = 1\n",
    );
    let original_settings = std::fs::read(harness.settings_path()).unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let source = std::fs::canonicalize(foreign.path()).unwrap();
    let marker = root.join("MCP_MUST_NEVER_EXECUTE");
    write(
        &source,
        ".codex/config.toml",
        &format!(
            "[mcp_servers.import-reader]\ncommand='/usr/bin/touch'\nargs=['{}']\n",
            marker.display()
        ),
    );
    write(
        &source,
        ".codex/agents/import-explorer.toml",
        "name='Import explorer'\ndescription='Read code'\nsandbox_mode='read-only'\ndeveloper_instructions='FROZEN_AGENT_GUIDANCE'\n",
    );
    write(
        &source,
        ".agents/skills/import-review/SKILL.md",
        "---\nname: import-review\ndescription: Imported review\n---\nFROZEN_SKILL_GUIDANCE\n",
    );
    write(&source, ".codex/AGENTS.md", "IMPORTED_USER_GUIDANCE");
    write(
        &source,
        ".gemini/commands/import-echo.toml",
        "description='Echo arguments'\nprompt='FROZEN_COMMAND {{args}}'\n",
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "/unreadable/credential/source",
        source.join(".codex/auth.json"),
    )
    .unwrap();

    let capture = Arc::new(Capture::default());
    let mut first = harness.with_provider(capture.clone()).compose().unwrap();
    let first_agent = first
        .context()
        .get::<Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let output = capture_info(&first_agent);
    let trust_before = first
        .context()
        .get::<heycode_trust::WorkspaceTrustService>(heycode_trust::SERVICE_TRUST)
        .unwrap()
        .snapshot()
        .unwrap();
    import_ready(first.context(), &output, "codex", &source).await;
    assert_eq!(
        ImportStore::new(&native_home)
            .unwrap()
            .snapshot()
            .unwrap()
            .revision(),
        1
    );
    assert_eq!(imports(first.context()).active_generation().revision(), 0);
    assert!(
        first
            .context()
            .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
            .unwrap()
            .get("import-review")
            .unwrap()
            .is_none()
    );
    assert!(
        first
            .context()
            .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
            .unwrap()
            .preset("import-explorer")
            .is_none()
    );
    import_ready(first.context(), &output, "gemini", &source).await;
    import_ready(
        first.context(),
        &output,
        "gemini --project",
        &root.join("workspace"),
    )
    .await;
    assert!(
        capture.0.lock().unwrap().is_empty(),
        "the human import flow must not call inference"
    );
    assert!(!marker.exists());
    assert!(
        heycode_session::derive_messages(first_agent.session().lock().unwrap().events()).is_empty()
    );
    assert_eq!(
        trust_before.revision(),
        first
            .context()
            .get::<heycode_trust::WorkspaceTrustService>(heycode_trust::SERVICE_TRUST)
            .unwrap()
            .snapshot()
            .unwrap()
            .revision()
    );
    assert_eq!(
        std::fs::read(root.join("settings.toml")).unwrap(),
        original_settings
    );
    assert!(!native_home.join("agents/import-explorer.json").exists());
    assert!(!native_home.join("skills/import-review").exists());
    assert!(
        !output
            .lock()
            .unwrap()
            .join("\n")
            .contains("FROZEN_AGENT_GUIDANCE")
    );
    first.context_mut().shutdown();

    // Source changes after publication cannot change frozen imported payloads.
    write(
        &source,
        ".codex/AGENTS.md",
        "UNREVIEWED_NEW_SOURCE_GUIDANCE",
    );
    write(
        &source,
        ".gemini/commands/import-echo.toml",
        "prompt='UNREVIEWED_NEW_COMMAND'\n",
    );
    let mut second = recompose(&root, &config, capture.clone());
    let active = imports(&second);
    assert_eq!(active.active_generation().revision(), 3);
    let agents = second
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let preset = agents.preset("import-explorer").unwrap();
    assert_eq!(preset.instructions(), "FROZEN_AGENT_GUIDANCE");
    assert_eq!(
        preset.config().permissions,
        heycode_agent::ChildPermissions::ReadOnly
    );
    assert_eq!(
        second
            .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
            .unwrap()
            .get_enabled("import-review")
            .unwrap()
            .unwrap()
            .body
            .trim(),
        "FROZEN_SKILL_GUIDANCE"
    );
    let settings = second
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap()
        .get(&heycode_settings::SettingsNamespace::new("mcp-servers").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        settings.user().unwrap()["servers"]["import-reader"]["enabled"],
        false
    );
    assert!(!marker.exists());
    let agent = second.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    let echo = second
        .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("import-echo")
        .unwrap()
        .unwrap();
    echo.execute(&agent, "raw \"quoted argument\"")
        .await
        .unwrap();
    let requests = capture.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let rendered = requests[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let positions = [
        "IMPORTED_USER_GUIDANCE",
        "NATIVE_USER_GUIDANCE",
        "IMPORTED_PROJECT_GUIDANCE",
        "NATIVE_PROJECT_GUIDANCE",
    ]
    .map(|marker| rendered.find(marker).unwrap());
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "incorrect instruction precedence: {rendered}"
    );
    assert!(rendered.contains("FROZEN_COMMAND raw \"quoted argument\""));
    assert!(!rendered.contains("UNREVIEWED_NEW"));
    drop(requests);
    assert_eq!(
        std::fs::read(root.join("settings.toml")).unwrap(),
        original_settings
    );
    assert!(!marker.exists());
    second.shutdown();
    first.shutdown();
}

#[tokio::test]
async fn production_host_rejects_native_changes_project_promotion_and_unsupported_project_mcp() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_trusted_workspace();
    let home = harness.credentials_root();
    let workspace = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    let source = tempfile::tempdir().unwrap();
    let source_root = std::fs::canonicalize(source.path()).unwrap();
    write(
        &source_root,
        ".codex/agents/example.toml",
        "name='Example'\ndescription='Read source'\ndeveloper_instructions='Imported instructions'\n",
    );
    write(
        &workspace,
        ".codex/config.toml",
        "[mcp_servers.example]\ncommand='example'\n",
    );
    let world = harness.compose().unwrap();
    let service = imports(world.context());
    assert!(matches!(
        service.discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root: workspace.clone(),
            target: ImportTarget::User
        }),
        Err(ImportError::Authority)
    ));
    let project = service
        .discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root: workspace.clone(),
            target: ImportTarget::project(&workspace).unwrap(),
        })
        .unwrap();
    assert!(
        project
            .items()
            .iter()
            .any(|row| row.status == ImportItemStatus::NeedsBinding
                && row.reason
                    == heycode_extension_host::config_import::ImportItemReason::ScopeBinding)
    );
    let inventory = service
        .discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root,
            target: ImportTarget::User,
        })
        .unwrap();
    let decisions = vec![ConfigImportDecision::Add {
        item_id: inventory.items()[0].id.clone(),
    }];
    let plan = service.prepare(&inventory, &decisions).unwrap();
    let digest = plan.preview().digest.clone();
    let confirmed = service.confirm_human(plan, &digest).unwrap();
    write(
        &home,
        "agents/example.json",
        "{\"display\":\"native\",\"instructions\":\"Native instructions\"}",
    );
    assert!(matches!(
        service.commit(&confirmed, &CancellationToken::new()),
        Err(ImportError::Stale)
    ));
    assert_eq!(
        ImportStore::new(&home)
            .unwrap()
            .snapshot()
            .unwrap()
            .revision(),
        0
    );
    assert!(matches!(
        service.prepare(&inventory, &decisions),
        Err(ImportError::Conflict)
    ));
    world.shutdown();
}

#[tokio::test]
async fn imported_project_resources_remain_inactive_in_another_workspace() {
    let harness = RealCompositionHarness::new()
        .unwrap()
        .with_trusted_workspace();
    let home = harness.credentials_root();
    let workspace = std::fs::canonicalize(harness.root().join("workspace")).unwrap();
    write(&workspace, "GEMINI.md", "PROJECT_ONLY_IMPORT");
    let world = harness.compose().unwrap();
    let service = imports(world.context());
    let inventory = service
        .discover(&ConfigImportRequest {
            product: ImportProduct::Gemini,
            source_root: workspace.clone(),
            target: ImportTarget::project(&workspace).unwrap(),
        })
        .unwrap();
    let plan = service
        .prepare(
            &inventory,
            &[ConfigImportDecision::Add {
                item_id: inventory.items()[0].id.clone(),
            }],
        )
        .unwrap();
    let digest = plan.preview().digest.clone();
    let confirmed = service.confirm_human(plan, &digest).unwrap();
    assert!(matches!(
        service
            .commit(&confirmed, &CancellationToken::new())
            .unwrap(),
        ImportCommitOutcome::Committed(_)
    ));
    let elsewhere = tempfile::tempdir().unwrap();
    let trust = heycode_trust::WorkspaceTrustService::memory(
        elsewhere.path(),
        heycode_cli::project_content_policy(),
    )
    .unwrap();
    let mount = heycode_extension_host::config_import::PinnedImportMount::new(
        Arc::new(ImportStore::new(&home).unwrap().snapshot().unwrap()),
        trust,
    )
    .unwrap();
    assert!(mount.keys().is_empty());
    assert!(!mount.has_project_resources());
    world.shutdown();
}
