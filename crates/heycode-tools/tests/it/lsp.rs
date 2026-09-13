//! E08 model-facing LSP Consumers and E06 spill integration.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_exec::{
    FileSystemPolicy, FileSystemRoot, FileSystemRootAccess, FileSystemService, LspBackend,
    LspCallHierarchyEdge, LspDiagnostic, LspDiagnosticSeverity, LspDocumentRequest, LspError,
    LspErrorCode, LspHover, LspLocation, LspPosition, LspPositionRequest, LspRange,
    LspServerDefinition, LspServerDescriptor, LspServerId, LspService, LspSymbol,
    LspWorkspaceSymbolRequest, ResolvedPath, RetainedOutputConfig, RetainedOutputOwner,
    RetainedOutputService,
};
use heycode_tools::{Tool, ToolCtx, lsp_tools};
use tokio_util::sync::CancellationToken;

struct FixtureLsp {
    file: ResolvedPath,
}

#[async_trait]
impl LspBackend for FixtureLsp {
    fn register_effect(
        &self,
        _context: &heycode_core::Context,
        _definition: LspServerDefinition,
    ) -> Result<(), LspError> {
        Ok(())
    }

    fn servers(&self) -> Result<Vec<LspServerDescriptor>, LspError> {
        Ok(vec![LspServerDescriptor::new(
            LspServerId::new("rust-fixture")?,
            "rust",
        )?])
    }

    async fn definition(
        &self,
        _request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        if cancellation.is_cancelled() {
            return Err(LspError::from_code(LspErrorCode::Cancelled));
        }
        Ok(vec![LspLocation::new(
            self.file.clone(),
            LspRange::new(LspPosition::new(2, 3), LspPosition::new(2, 7))?,
        )])
    }

    async fn references(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        self.definition(request, cancellation).await
    }

    async fn hover(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Option<LspHover>, LspError> {
        Ok(Some(LspHover::new("fn fixture()", None)?))
    }

    async fn implementations(
        &self,
        request: LspPositionRequest,
        cancellation: CancellationToken,
    ) -> Result<Vec<LspLocation>, LspError> {
        self.definition(request, cancellation).await
    }

    async fn document_symbols(
        &self,
        _request: LspDocumentRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        Ok(vec![fixture_symbol(self.file.clone(), "fixture")?])
    }

    async fn workspace_symbols(
        &self,
        _request: LspWorkspaceSymbolRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspSymbol>, LspError> {
        Ok(vec![fixture_symbol(
            self.file.clone(),
            "workspace_fixture",
        )?])
    }

    async fn incoming_calls(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        Ok(vec![LspCallHierarchyEdge::new(
            fixture_symbol(self.file.clone(), "caller")?,
            vec![LspRange::new(
                LspPosition::new(1, 0),
                LspPosition::new(1, 4),
            )?],
        )])
    }

    async fn outgoing_calls(
        &self,
        _request: LspPositionRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspCallHierarchyEdge>, LspError> {
        Ok(vec![LspCallHierarchyEdge::new(
            fixture_symbol(self.file.clone(), "callee")?,
            Vec::new(),
        )])
    }

    async fn diagnostics(
        &self,
        _request: LspDocumentRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let mut rows = Vec::new();
        for line in 0..40 {
            rows.push(LspDiagnostic::new(
                self.file.clone(),
                LspRange::new(LspPosition::new(line, 0), LspPosition::new(line, 1))?,
                LspDiagnosticSeverity::Warning,
                "x".repeat(4_000),
                Some("fixture".to_owned()),
            )?);
        }
        Ok(rows)
    }

    fn close(&self) {}
}

fn fixture_symbol(path: ResolvedPath, name: &str) -> Result<LspSymbol, LspError> {
    let range = LspRange::new(LspPosition::new(2, 3), LspPosition::new(2, 7))?;
    LspSymbol::new(
        name,
        Some("fixture detail".to_owned()),
        12,
        path,
        range,
        range,
        None,
    )
}

fn filesystem(root: &std::path::Path) -> Arc<FileSystemService> {
    Arc::new(
        FileSystemService::local(
            FileSystemPolicy::new([
                FileSystemRoot::new(root, FileSystemRootAccess::ReadWrite).unwrap()
            ])
            .unwrap(),
        )
        .unwrap(),
    )
}

fn tool<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools
        .iter()
        .find(|tool| tool.spec().name == name)
        .expect("tool registered")
}

#[cfg(unix)]
#[tokio::test]
async fn listing_definition_and_large_diagnostics_cross_the_real_tool_and_spill_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("main.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();
    let resolved = ResolvedPath::new(path).unwrap();
    let retained = Arc::new(
        RetainedOutputService::open(
            RetainedOutputConfig::new(root.join("retained-output")).unwrap(),
        )
        .unwrap(),
    );
    let tools = lsp_tools(
        Arc::new(LspService::new(Arc::new(FixtureLsp {
            file: resolved.clone(),
        }))),
        filesystem(&root),
        retained,
        RetainedOutputOwner::new("fixture").unwrap(),
    );
    let cx = ToolCtx::default().with_cwd(root.clone());

    assert_eq!(
        tool(&tools, "lsp_servers")
            .run(serde_json::json!({}), &cx)
            .await
            .unwrap(),
        serde_json::json!([{"id":"rust-fixture","language":"rust"}])
    );
    let definition = tool(&tools, "lsp_definition")
        .run(
            serde_json::json!({
                "server":"rust-fixture","path":"main.rs","line":0,"character":3
            }),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(
        tool(&tools, "lsp_definition")
            .untrusted_content()
            .unwrap()
            .source(),
        heycode_core::UntrustedContentSource::Lsp
    );
    assert_eq!(definition[0]["range"]["start"]["line"], 2);
    assert_eq!(definition[0]["range"]["start"]["character"], 3);

    let unified = tool(&tools, "lsp");
    assert_eq!(
        unified
            .run(serde_json::json!({"operation":"servers"}), &cx)
            .await
            .unwrap()[0]["id"],
        "rust-fixture"
    );
    for operation in ["definition", "references", "implementations"] {
        let result = unified
            .run(
                serde_json::json!({
                    "operation":operation,"server":"rust-fixture","path":"main.rs",
                    "line":0,"character":3
                }),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(result[0]["range"]["start"]["line"], 2, "{operation}");
    }
    let hover = unified
        .run(
            serde_json::json!({
                "operation":"hover","server":"rust-fixture","path":"main.rs",
                "line":0,"character":3
            }),
            &cx,
        )
        .await
        .unwrap();
    assert_eq!(hover["contents"], "fn fixture()");
    for (operation, expected) in [
        ("document_symbols", "fixture"),
        ("workspace_symbols", "workspace_fixture"),
    ] {
        let result = unified
            .run(
                serde_json::json!({
                    "operation":operation,"server":"rust-fixture","path":"main.rs",
                    "query":"fixture"
                }),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(result[0]["name"], expected, "{operation}");
    }
    for (operation, expected) in [("incoming_calls", "caller"), ("outgoing_calls", "callee")] {
        let result = unified
            .run(
                serde_json::json!({
                    "operation":operation,"server":"rust-fixture","path":"main.rs",
                    "line":0,"character":3
                }),
                &cx,
            )
            .await
            .unwrap();
        assert_eq!(result[0]["symbol"]["name"], expected, "{operation}");
    }
    assert!(
        unified
            .run(
                serde_json::json!({
                    "operation":"diagnostics","server":"rust-fixture","path":"main.rs"
                }),
                &cx,
            )
            .await
            .unwrap()
            .as_str()
            .is_some(),
        "large unified diagnostic result must retain rather than truncate"
    );

    let diagnostics = tool(&tools, "lsp_diagnostics")
        .run(
            serde_json::json!({"server":"rust-fixture","path":"main.rs"}),
            &cx,
        )
        .await
        .unwrap();
    let preview = diagnostics.as_str().expect("large result spills");
    assert!(preview.starts_with("[retained-output"), "{preview}");
    assert!(preview.len() <= heycode_exec::MAX_RETAINED_OUTPUT_VIEW_BYTES);
    assert!(preview.contains("sha256-"));
}
