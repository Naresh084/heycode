#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use heycode_config::imports::{ImportCommitOutcome, ImportGeneration, ImportStore};
use heycode_settings::{SettingsDocuments, SettingsNamespace};
use heycode_settings_file::{FileSettingsConfig, FileSettingsProvider};
use tokio_util::sync::CancellationToken;

use super::*;

#[derive(Default)]
struct Host {
    revision: AtomicU64,
    conflicts: Mutex<BTreeSet<ImportResourceKey>>,
}
impl ConfigImportHost for Host {
    fn snapshot(
        &self,
        target: &ImportTarget,
        _keys: &[ImportResourceKey],
    ) -> Result<ImportHostSnapshot, ImportError> {
        target.recheck()?;
        ImportHostSnapshot::new(
            self.revision.load(Ordering::SeqCst).to_string(),
            self.conflicts.lock().unwrap().clone(),
        )
    }
}

struct Fixture {
    root: tempfile::TempDir,
    source: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    host: Arc<Host>,
    service: ConfigImportService,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source-home");
        let home = root.path().join("native-home");
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let host = Arc::new(Host::default());
        let service = ConfigImportService::new(
            ImportStore::new(&home).unwrap(),
            host.clone(),
            Arc::new(ImportGeneration::default()),
        );
        Self {
            root,
            source,
            home,
            workspace,
            host,
            service,
        }
    }
    fn write(&self, relative: &str, text: &str) {
        let path = self.source.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn scan(&self, product: ImportProduct) -> ConfigImportInventory {
        self.service
            .discover(&ConfigImportRequest {
                product,
                source_root: self.source.clone(),
                target: ImportTarget::User,
            })
            .unwrap()
    }
    fn prepare_ready(&self, inventory: &ConfigImportInventory) -> PreparedConfigImport {
        let decisions = inventory
            .items()
            .iter()
            .filter(|row| {
                matches!(
                    row.status,
                    ImportItemStatus::Ready | ImportItemStatus::Duplicate
                )
            })
            .map(|row| ConfigImportDecision::Add {
                item_id: row.id.clone(),
            })
            .collect::<Vec<_>>();
        self.service.prepare(inventory, &decisions).unwrap()
    }
    fn confirm(&self, prepared: PreparedConfigImport) -> ConfirmedConfigImport {
        let digest = prepared.preview().digest.clone();
        self.service.confirm_human(prepared, &digest).unwrap()
    }
    fn generation(&self) -> Arc<ImportGeneration> {
        Arc::new(ImportStore::new(&self.home).unwrap().snapshot().unwrap())
    }
    fn trust(&self, trusted: bool) -> heycode_trust::WorkspaceTrustService {
        let trust = heycode_trust::WorkspaceTrustService::memory(
            &self.workspace,
            heycode_trust::ProjectContentPolicy::new(
                heycode_trust::UntrustedProjectAccess::Block,
                heycode_trust::UntrustedProjectAccess::Block,
            ),
        )
        .unwrap();
        if trusted {
            trust
                .set_session(
                    heycode_trust::WorkspaceTrustDecision::Trusted,
                    trust.snapshot().unwrap().revision(),
                )
                .unwrap();
        }
        trust
    }
}

const AGENT: &str = "name = 'Explorer'\ndescription = 'Read-only source explorer'\nsandbox_mode = 'read-only'\ndeveloper_instructions = 'Inspect the local implementation.'\n";

#[test]
fn codex_ready_resources_commit_and_activate_through_native_reader_types() {
    let fixture = Fixture::new();
    fixture.write(".codex/agents/explorer.toml", AGENT);
    fixture.write(".agents/skills/review/SKILL.md", "---\nname: review\ndescription: Review source carefully\n---\nRead the relevant implementation.\n");
    fixture.write(".codex/AGENTS.md", "Preserve task-specific constraints.");
    fixture.write(".codex/auth.json", "THIS_CREDENTIAL_STORE_MUST_NOT_BE_READ");
    let inventory = fixture.scan(ImportProduct::Codex);
    assert_eq!(inventory.items().len(), 3);
    assert!(
        inventory
            .items()
            .iter()
            .all(|row| row.status == ImportItemStatus::Ready)
    );
    assert!(
        inventory
            .inputs
            .files
            .iter()
            .all(|(_, path, _)| !path.ends_with("auth.json"))
    );
    assert!(!fixture.home.exists());
    let plan = fixture.prepare_ready(&inventory);
    assert_eq!(plan.preview().actions.len(), 3);
    assert!(!format!("{plan:?}").contains("Inspect the local implementation"));
    let confirmed = fixture.confirm(plan);
    assert!(matches!(
        fixture
            .service
            .commit(&confirmed, &CancellationToken::new())
            .unwrap(),
        ImportCommitOutcome::Committed(_)
    ));
    assert_eq!(fixture.service.active_generation().revision(), 0);
    let mount = PinnedImportMount::new(fixture.generation(), fixture.trust(false)).unwrap();
    let presets = mount.agent_presets().unwrap();
    assert_eq!(presets[0].id().as_str(), "user-explorer");
    assert_eq!(
        presets[0].config().permissions,
        heycode_agent::ChildPermissions::ReadOnly
    );
    let roots = crate::user_declarations::UserDeclarationRoots {
        user_home: Some(fixture.home.clone()),
        workspace: None,
    };
    let loaded = crate::user_declarations::load_user_declarations_with_imports(&roots, &presets);
    assert!(
        loaded
            .presets
            .iter()
            .any(|preset| preset.id().as_str() == "explorer")
    );
    assert!(
        !fixture.home.join("agents").exists(),
        "import must not copy native agent files"
    );
    let duplicate = fixture.prepare_ready(&fixture.scan(ImportProduct::Codex));
    assert_eq!(duplicate.preview().duplicates, 3);
    let before = std::fs::read(fixture.home.join("state/config-imports/current.json")).unwrap();
    assert_eq!(
        fixture
            .service
            .commit(&fixture.confirm(duplicate), &CancellationToken::new())
            .unwrap(),
        ImportCommitOutcome::Unchanged { revision: 1 }
    );
    assert_eq!(
        std::fs::read(fixture.home.join("state/config-imports/current.json")).unwrap(),
        before
    );
}

#[test]
fn source_tree_source_bytes_host_revision_and_confirmation_are_revalidated() {
    let fixture = Fixture::new();
    fixture.write(".codex/agents/explorer.toml", AGENT);
    let inventory = fixture.scan(ImportProduct::Codex);
    let plan = fixture.prepare_ready(&inventory);
    assert!(matches!(
        fixture
            .service
            .confirm_human(plan, "not-the-reviewed-digest"),
        Err(ImportError::ConfirmationRequired)
    ));
    let confirmed = fixture.confirm(fixture.prepare_ready(&inventory));
    fixture.write(".codex/agents/new.toml", AGENT);
    assert_eq!(
        fixture
            .service
            .commit(&confirmed, &CancellationToken::new()),
        Err(ImportError::Stale)
    );
    assert!(
        !fixture
            .home
            .join("state/config-imports/current.json")
            .exists()
    );
    std::fs::remove_file(fixture.source.join(".codex/agents/new.toml")).unwrap();
    fixture.write(
        ".codex/agents/explorer.toml",
        &AGENT.replace("local", "changed"),
    );
    assert_eq!(
        fixture
            .service
            .commit(&confirmed, &CancellationToken::new()),
        Err(ImportError::Stale)
    );
    fixture.write(".codex/agents/explorer.toml", AGENT);
    let fresh = fixture.confirm(fixture.prepare_ready(&fixture.scan(ImportProduct::Codex)));
    fixture.host.revision.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        fixture.service.commit(&fresh, &CancellationToken::new()),
        Err(ImportError::Stale)
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        fixture.service.commit(&fresh, &cancelled).unwrap(),
        ImportCommitOutcome::CancelledBeforePublication
    );
    fixture.service.close();
    assert!(matches!(
        fixture.service.discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root: fixture.source.clone(),
            target: ImportTarget::User
        }),
        Err(ImportError::Authority)
    ));
}

#[test]
fn explicit_rename_and_keep_do_not_replace_native_conflicts() {
    let fixture = Fixture::new();
    fixture.write(".codex/agents/explorer.toml", AGENT);
    fixture
        .host
        .conflicts
        .lock()
        .unwrap()
        .insert(ImportResourceKey {
            kind: ImportResourceKind::Agent,
            name: "explorer".to_owned(),
        });
    let inventory = fixture.scan(ImportProduct::Codex);
    assert_eq!(inventory.items()[0].status, ImportItemStatus::Conflict);
    let id = inventory.items()[0].id.clone();
    assert!(matches!(
        fixture.service.prepare(
            &inventory,
            &[ConfigImportDecision::Add {
                item_id: id.clone()
            }]
        ),
        Err(ImportError::Conflict)
    ));
    assert!(matches!(
        fixture.service.prepare(
            &inventory,
            &[ConfigImportDecision::KeepExisting {
                item_id: id.clone()
            }]
        ),
        Err(ImportError::InvalidDocument)
    ));
    let plan = fixture
        .service
        .prepare(
            &inventory,
            &[ConfigImportDecision::Rename {
                item_id: id,
                name: "imported-explorer".to_owned(),
            }],
        )
        .unwrap();
    fixture
        .service
        .commit(&fixture.confirm(plan), &CancellationToken::new())
        .unwrap();
    assert_eq!(
        fixture.generation().entries()[0].resource().name(),
        "imported-explorer"
    );
    assert!(!fixture.home.join("agents").exists());
}

#[test]
fn gemini_cursor_transports_and_dynamic_semantics_have_explicit_omissions() {
    let fixture = Fixture::new();
    fixture.write(".gemini/settings.json", r#"{"model":{"name":"example"},"mcpServers":{"http":{"httpUrl":"https://example.invalid/mcp"},"legacy":{"url":"https://example.invalid/sse"},"credential":{"command":"server","env":{"TOKEN":"SECRET_SENTINEL"}},"filtered":{"command":"server","includeTools":["read"]}}}"#);
    fixture.write(
        ".gemini/commands/plain.toml",
        "prompt = 'Review {{args}} carefully.'\ndescription = 'Review a supplied target'\n",
    );
    fixture.write(
        ".gemini/commands/shell.toml",
        "prompt = 'Read !{cat data}'\n",
    );
    fixture.write(".gemini/GEMINI.md", "@./private-instructions.md\n");
    let inventory = fixture.scan(ImportProduct::Gemini);
    assert_eq!(
        inventory
            .items()
            .iter()
            .filter(|row| row.status == ImportItemStatus::Ready)
            .count(),
        2
    );
    assert!(
        inventory
            .items()
            .iter()
            .any(|row| row.reason == ImportItemReason::TransportBinding)
    );
    assert!(
        inventory
            .items()
            .iter()
            .any(|row| row.reason == ImportItemReason::CredentialOrInterpolation)
    );
    assert!(
        inventory
            .items()
            .iter()
            .any(|row| row.reason == ImportItemReason::ExternalDependency)
    );
    assert!(
        !serde_json::to_string(inventory.items())
            .unwrap()
            .contains("SECRET_SENTINEL")
    );
    let plan = fixture.prepare_ready(&inventory);
    assert!(
        !serde_json::to_string(plan.preview())
            .unwrap()
            .contains("SECRET_SENTINEL")
    );
    fixture
        .service
        .commit(&fixture.confirm(plan), &CancellationToken::new())
        .unwrap();
    let mount =
        Arc::new(PinnedImportMount::new(fixture.generation(), fixture.trust(false)).unwrap());
    let settings_path = fixture.root.path().join("settings.toml");
    let provider =
        FileSettingsProvider::new(FileSettingsConfig::user(&settings_path).without_watch())
            .with_pinned_overlay(mount.clone());
    let namespace = SettingsNamespace::new("mcp-servers").unwrap();
    assert_eq!(
        provider
            .load_documents()
            .unwrap()
            .user_section(&namespace)
            .unwrap()["servers"]["http"]["enabled"],
        false
    );
    assert!(!settings_path.exists());
    std::fs::write(
        &settings_path,
        "[settings.mcp-servers.servers.http]\nenabled = true\n",
    )
    .unwrap();
    let documents = provider.load_documents().unwrap();
    let row = &documents.user_section(&namespace).unwrap()["servers"]["http"];
    assert_eq!(row["enabled"], true);
    assert!(
        row.get("target").is_none(),
        "native fragments never activate inherited transport bytes"
    );
    assert!(!mount.diagnostics().is_empty());
    let mut higher_native = SettingsDocuments::new();
    higher_native
        .set_project(
            namespace.clone(),
            serde_json::json!({"servers":{"http":{"enabled":true}}}),
        )
        .unwrap();
    let merged =
        heycode_settings_file::PinnedSettingsOverlay::merge(mount.as_ref(), higher_native).unwrap();
    assert!(
        merged.user_section(&namespace).unwrap()["servers"]
            .get("http")
            .is_none(),
        "higher-scope native fragments must never inherit imported command bytes"
    );
    fixture.write(".cursor/mcp.json", r#"{"mcpServers":{"ambiguous":{"url":"https://example.invalid/mcp"},"stdio":{"command":"server","args":["--local"]}}}"#);
    let cursor = fixture.scan(ImportProduct::Cursor);
    assert_eq!(
        cursor
            .items()
            .iter()
            .filter(|row| row.status == ImportItemStatus::Ready)
            .count(),
        1
    );
    assert_eq!(
        cursor
            .items()
            .iter()
            .filter(|row| row.reason == ImportItemReason::TransportBinding)
            .count(),
        1
    );
}

#[test]
fn project_identity_trust_and_native_agent_scope_precedence_are_preserved() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.workspace.join(".codex/agents")).unwrap();
    std::fs::write(fixture.workspace.join(".codex/agents/explorer.toml"), AGENT).unwrap();
    let target = ImportTarget::project(&fixture.workspace).unwrap();
    let inventory = fixture
        .service
        .discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root: fixture.workspace.clone(),
            target: target.clone(),
        })
        .unwrap();
    let plan = fixture.prepare_ready(&inventory);
    assert_eq!(plan.preview().scope, "project");
    fixture
        .service
        .commit(&fixture.confirm(plan), &CancellationToken::new())
        .unwrap();
    assert!(
        PinnedImportMount::new(fixture.generation(), fixture.trust(false))
            .unwrap()
            .agent_presets()
            .unwrap()
            .is_empty()
    );
    let trusted = PinnedImportMount::new(fixture.generation(), fixture.trust(true)).unwrap();
    assert!(trusted.has_project_resources());
    let imported = trusted.agent_presets().unwrap();
    assert_eq!(imported[0].id().as_str(), "project-explorer");
    std::fs::create_dir_all(fixture.home.join("agents")).unwrap();
    std::fs::write(
        fixture.home.join("agents/explorer.json"),
        r#"{"display":"Native user","instructions":"Native user instructions"}"#,
    )
    .unwrap();
    let roots = crate::user_declarations::UserDeclarationRoots {
        user_home: Some(fixture.home.clone()),
        workspace: Some(crate::user_declarations::WorkspaceDeclarationAccess {
            root: fixture.workspace.clone(),
            executables_allowed: false,
        }),
    };
    let loaded = crate::user_declarations::load_user_declarations_with_imports(&roots, &imported);
    assert_eq!(
        loaded
            .presets
            .iter()
            .find(|preset| preset.id().as_str() == "explorer")
            .unwrap()
            .display(),
        "Explorer"
    );
    std::fs::create_dir_all(fixture.workspace.join(".heycode/agents")).unwrap();
    std::fs::write(
        fixture.workspace.join(".heycode/agents/explorer.json"),
        r#"{"display":"Native project","instructions":"Native project instructions"}"#,
    )
    .unwrap();
    let loaded = crate::user_declarations::load_user_declarations_with_imports(&roots, &imported);
    assert_eq!(
        loaded
            .presets
            .iter()
            .find(|preset| preset.id().as_str() == "explorer")
            .unwrap()
            .display(),
        "Native project"
    );
    assert!(
        loaded
            .skipped
            .iter()
            .any(|row| row.reason.contains("precedence"))
    );
    assert!(matches!(
        fixture.service.discover(&ConfigImportRequest {
            product: ImportProduct::Codex,
            source_root: fixture.source.clone(),
            target
        }),
        Err(ImportError::Authority)
    ));
    let before = fixture.generation();
    std::fs::rename(
        &fixture.workspace,
        fixture.root.path().join("old-workspace"),
    )
    .unwrap();
    std::fs::create_dir(&fixture.workspace).unwrap();
    assert!(matches!(
        PinnedImportMount::new(before, fixture.trust(true)),
        Err(ImportError::Stale)
    ));
}

#[test]
fn unsupported_codex_skill_activation_metadata_blocks_dependent_skills() {
    let fixture = Fixture::new();
    fixture.write(
        ".codex/config.toml",
        "[[skills.config]]\npath = '/not-opened/SKILL.md'\nenabled = false\n",
    );
    fixture.write(
        ".agents/skills/example/SKILL.md",
        "---\nname: example\ndescription: Example guidance\n---\nUse local evidence.\n",
    );
    let inventory = fixture.scan(ImportProduct::Codex);
    assert!(
        inventory
            .items()
            .iter()
            .all(|row| row.status != ImportItemStatus::Ready)
    );
    assert!(
        inventory
            .items()
            .iter()
            .any(|row| row.reason == ImportItemReason::ActivationSemantics)
    );
    assert!(!fixture.home.exists());
    let empty = SettingsDocuments::new();
    let _ = empty;
}
