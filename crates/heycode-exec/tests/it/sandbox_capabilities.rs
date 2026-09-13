//! E10 effective sandbox choice and guarantee report contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_exec::{
    FileReadScope, FileWriteScope, NetworkScope, Sandbox, SandboxBackendCapabilities, SandboxError,
    SandboxMode, SandboxPolicy, SandboxService, SandboxSupport,
};

struct Backend {
    capabilities: SandboxBackendCapabilities,
}

impl Sandbox for Backend {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        self.capabilities.clone()
    }

    fn confine(
        &self,
        argv: &[String],
        _policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        Ok(argv.to_vec())
    }
}

fn capabilities(read_only: SandboxSupport, workspace: SandboxSupport) -> Arc<dyn Sandbox> {
    Arc::new(Backend {
        capabilities: SandboxBackendCapabilities {
            read_only,
            workspace_write: workspace,
            network_isolation: SandboxSupport::Unsupported,
        },
    })
}

#[test]
fn off_with_an_available_backend_reports_all_truthful_choices() {
    let root = tempfile::tempdir().unwrap();
    let service = SandboxService::new(
        SandboxMode::Off,
        root.path().canonicalize().unwrap(),
        Some(capabilities(
            SandboxSupport::Supported,
            SandboxSupport::Supported,
        )),
    )
    .unwrap();
    let report = service.capability_report();
    assert_eq!(report.effective_mode, SandboxMode::Off);
    assert_eq!(report.active_backend, None);
    assert_eq!(report.available_backend, Some("fixture"));

    let full = report.choice(SandboxMode::Off).unwrap();
    assert!(full.selectable);
    assert_eq!(full.file_read, FileReadScope::Host);
    assert_eq!(full.file_write, FileWriteScope::Host);
    assert_eq!(full.network, NetworkScope::Host);

    let read_only = report.choice(SandboxMode::ReadOnly).unwrap();
    assert!(read_only.selectable);
    assert_eq!(read_only.file_read, FileReadScope::Host);
    assert_eq!(read_only.file_write, FileWriteScope::DeviceOnly);
    assert_eq!(read_only.network, NetworkScope::Host);

    let workspace = report.choice(SandboxMode::WorkspaceWrite).unwrap();
    assert!(workspace.selectable);
    assert_eq!(workspace.file_read, FileReadScope::Host);
    assert_eq!(workspace.file_write, FileWriteScope::WorkspaceAndTemp);
    assert_eq!(workspace.network, NetworkScope::Host);
}

#[test]
fn absent_or_incapable_backend_makes_unsupported_choices_unselectable() {
    let root = tempfile::tempdir().unwrap();
    let off = SandboxService::new(SandboxMode::Off, root.path().canonicalize().unwrap(), None)
        .unwrap()
        .capability_report();
    assert!(off.choice(SandboxMode::Off).unwrap().selectable);
    assert!(!off.choice(SandboxMode::ReadOnly).unwrap().selectable);
    assert!(!off.choice(SandboxMode::WorkspaceWrite).unwrap().selectable);

    let incapable = capabilities(SandboxSupport::Supported, SandboxSupport::Unsupported);
    assert!(
        SandboxService::new(
            SandboxMode::WorkspaceWrite,
            root.path().canonicalize().unwrap(),
            Some(incapable)
        )
        .is_err(),
        "an unenforceable effective mode must fail before publication"
    );
}

#[test]
fn choice_identifiers_are_stable_and_distinguish_full_access_from_confinement() {
    assert_eq!(SandboxMode::Off.as_str(), "full_access");
    assert_eq!(SandboxMode::ReadOnly.as_str(), "read_only");
    assert_eq!(SandboxMode::WorkspaceWrite.as_str(), "workspace_write");
}
