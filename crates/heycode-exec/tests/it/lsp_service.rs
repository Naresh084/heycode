//! E08 effect-owned LSP registry and local stdio provider.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use heycode_core::{Context, compose};
use heycode_exec::{
    FileSystemService, LspDiagnosticSeverity, LspDocumentRequest, LspErrorCode, LspPosition,
    LspPositionRequest, LspRange, LspServerDefinition, LspServerId, LspService,
    LspWorkspaceSymbolRequest, PathRequest, ProcessSpec, ResolvedPath, SERVICE_LSP, SandboxMode,
    SandboxService, local_filesystem_plugin, local_subprocess_plugin, lsp_registry_plugin,
    lsp_stdio_plugin, sandbox_service_plugin,
};
use tokio_util::sync::CancellationToken;

const HELPER_ENV: &str = "HEYCODE_EXEC_LSP_HELPER";
const MODE_ENV: &str = "HEYCODE_EXEC_LSP_MODE";
const READY_ENV: &str = "HEYCODE_EXEC_LSP_READY";
const REQUEST_ENV: &str = "HEYCODE_EXEC_LSP_REQUEST";
const CANCEL_ENV: &str = "HEYCODE_EXEC_LSP_CANCEL";
const RELEASE_ENV: &str = "HEYCODE_EXEC_LSP_RELEASE";
const SURVIVED_ENV: &str = "HEYCODE_EXEC_LSP_SURVIVED";
const REAL_CLANGD_ENV: &str = "HEYCODE_REAL_LSP_CLANGD";

fn server_id() -> LspServerId {
    LspServerId::new("fixture-lsp").unwrap()
}

fn helper_spec(root: &Path, mode: &str) -> ProcessSpec {
    ProcessSpec::new(std::env::current_exe().unwrap(), root)
        .unwrap()
        .with_args([
            OsString::from("--exact"),
            OsString::from(super::test_name(module_path!(), "lsp_helper_process")),
            OsString::from("--nocapture"),
        ])
        .unwrap()
        .with_environment([
            (OsString::from(HELPER_ENV), OsString::from("1")),
            (OsString::from(MODE_ENV), OsString::from(mode)),
            (
                OsString::from(READY_ENV),
                root.join("lsp-ready").into_os_string(),
            ),
            (
                OsString::from(REQUEST_ENV),
                root.join("lsp-request").into_os_string(),
            ),
            (
                OsString::from(CANCEL_ENV),
                root.join("lsp-cancel").into_os_string(),
            ),
            (
                OsString::from(RELEASE_ENV),
                root.join("release-descendant").into_os_string(),
            ),
            (
                OsString::from(SURVIVED_ENV),
                root.join("lsp-descendant-survived").into_os_string(),
            ),
        ])
        .unwrap()
        .with_interactive_stdio()
}

fn definition(root: &Path, mode: &str) -> LspServerDefinition {
    LspServerDefinition::new(
        server_id(),
        "rust",
        ResolvedPath::new(root).unwrap(),
        helper_spec(root, mode),
    )
    .unwrap()
}

fn world(root: &Path, mode: &str) -> (Context, std::sync::Arc<LspService>, ResolvedPath) {
    let sandbox = SandboxService::new(SandboxMode::Off, root, None).unwrap();
    let plugins = vec![
        sandbox_service_plugin(sandbox),
        local_subprocess_plugin(),
        local_filesystem_plugin(),
        lsp_registry_plugin(),
        lsp_stdio_plugin(vec![definition(root, mode)]),
    ];
    let context = compose(&plugins).unwrap();
    let lsp = context.get::<LspService>(SERVICE_LSP).unwrap();
    let filesystem = context
        .get::<FileSystemService>(heycode_exec::SERVICE_FILESYSTEM)
        .unwrap();
    let file = filesystem
        .resolve(PathRequest::new(root, "src/main.rs").unwrap())
        .unwrap();
    (context, lsp, file)
}

fn production_clangd_world(
    root: &Path,
    executable: &Path,
) -> (Context, std::sync::Arc<LspService>, ResolvedPath) {
    let server = LspServerId::new("production-clangd").unwrap();
    let process = ProcessSpec::new(executable, root)
        .unwrap()
        .with_args([
            OsString::from("--background-index=0"),
            OsString::from("--clang-tidy=0"),
            OsString::from("--log=error"),
        ])
        .unwrap()
        .with_interactive_stdio();
    let definition =
        LspServerDefinition::new(server, "cpp", ResolvedPath::new(root).unwrap(), process).unwrap();
    let sandbox = SandboxService::new(SandboxMode::Off, root, None).unwrap();
    let plugins = vec![
        sandbox_service_plugin(sandbox),
        local_subprocess_plugin(),
        local_filesystem_plugin(),
        lsp_registry_plugin(),
        lsp_stdio_plugin(vec![definition]),
    ];
    let context = compose(&plugins).unwrap();
    let lsp = context.get::<LspService>(SERVICE_LSP).unwrap();
    let filesystem = context
        .get::<FileSystemService>(heycode_exec::SERVICE_FILESYSTEM)
        .unwrap();
    let file = filesystem
        .resolve(PathRequest::new(root, "main.cpp").unwrap())
        .unwrap();
    (context, lsp, file)
}

/// Opt-in compatibility canary against a production language server. The test
/// creates all source and compilation-database inputs under a fresh temporary
/// directory and reaches clangd only through the public `LspService`.
#[tokio::test]
#[ignore = "set HEYCODE_REAL_LSP_CLANGD to an absolute installed clangd path"]
#[allow(clippy::print_stderr)]
async fn installed_clangd_serves_navigation_through_the_public_service() {
    let executable = PathBuf::from(std::env::var_os(REAL_CLANGD_ENV).expect(REAL_CLANGD_ENV));
    assert!(
        executable.is_absolute(),
        "{REAL_CLANGD_ENV} must be absolute"
    );
    let version = Command::new(&executable)
        .arg("--version")
        .output()
        .expect("clangd version probe must start");
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap();
    let version = version.lines().next().unwrap_or("unknown clangd");

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = concat!(
        "struct Base {\n",
        "  virtual int value() const = 0;\n",
        "};\n\n",
        "struct Derived : Base {\n",
        "  int value() const override { return 7; }\n",
        "};\n\n",
        "int twice(const Base& item) {\n",
        "  return item.value() * 2;\n",
        "}\n\n",
        "int main() {\n",
        "  Derived item;\n",
        "  return twice(item);\n",
        "}\n",
    );
    std::fs::write(root.join("main.cpp"), source).unwrap();
    let compile_commands = serde_json::json!([{
        "directory": root,
        "arguments": ["/usr/bin/clang++", "-std=c++17", "-c", "main.cpp"],
        "file": "main.cpp"
    }]);
    std::fs::write(
        root.join("compile_commands.json"),
        serde_json::to_vec_pretty(&compile_commands).unwrap(),
    )
    .unwrap();

    let (mut context, lsp, file) = production_clangd_world(&root, &executable);
    let server = LspServerId::new("production-clangd").unwrap();
    let token = CancellationToken::new();

    let definition = lsp
        .definition(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(14, 11)),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(
        definition
            .iter()
            .any(|location| { location.path() == &file && location.range().start().line() == 8 })
    );

    let references = lsp
        .references(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(8, 5)),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(references.len() >= 2);

    let hover = lsp
        .hover(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(13, 4)),
            token.clone(),
        )
        .await
        .unwrap()
        .expect("clangd should return hover for Derived");
    assert!(hover.contents().contains("Derived"));

    let implementations = lsp
        .implementations(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(1, 15)),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(
        implementations
            .iter()
            .any(|location| location.range().start().line() == 5)
    );

    let document_symbols = lsp
        .document_symbols(
            LspDocumentRequest::new(server.clone(), file.clone()),
            token.clone(),
        )
        .await
        .unwrap();
    for expected in ["Base", "Derived", "twice", "main"] {
        assert!(
            document_symbols
                .iter()
                .any(|symbol| symbol.name() == expected)
        );
    }

    let workspace_symbols = lsp
        .workspace_symbols(
            LspWorkspaceSymbolRequest::new(server.clone(), "twice").unwrap(),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(
        workspace_symbols
            .iter()
            .any(|symbol| symbol.name() == "twice")
    );

    let incoming = lsp
        .incoming_calls(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(8, 5)),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(incoming.iter().any(|edge| edge.symbol().name() == "main"));

    let diagnostics = lsp
        .diagnostics(
            LspDocumentRequest::new(server.clone(), file.clone()),
            token.clone(),
        )
        .await
        .unwrap();
    assert!(diagnostics.is_empty());

    let outgoing = lsp
        .outgoing_calls(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(12, 5)),
            token.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(outgoing.code(), LspErrorCode::Protocol);
    assert_eq!(outgoing.to_string(), "LSP server protocol failed");

    // This Apple clangd build advertises call hierarchy but returns JSON-RPC
    // method-not-found for outgoing calls. The adapter must expose a body-free
    // protocol class, retire that session, and recover on the next operation.
    let recovered = lsp
        .hover(
            LspPositionRequest::new(server.clone(), file.clone(), LspPosition::new(13, 4)),
            token,
        )
        .await
        .unwrap()
        .expect("clangd should recover after an unsupported operation");
    assert!(recovered.contents().contains("Derived"));

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = lsp
        .hover(
            LspPositionRequest::new(server, file, LspPosition::new(13, 4)),
            cancelled,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), LspErrorCode::Cancelled);

    eprintln!(
        "REAL_LSP_COMPAT version={version:?} definition={} references={} symbols={} workspace_symbols={} incoming={} diagnostics={} outgoing=protocol_unsupported recovery=ok precancel=cancelled root=<TEMP>",
        definition.len(),
        references.len(),
        document_symbols.len(),
        workspace_symbols.len(),
        incoming.len(),
        diagnostics.len(),
    );
    context.shutdown();
}

async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn all_navigation_operations_share_one_exact_stdio_server() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { let unused = 1; }\n").unwrap();
    let (mut context, lsp, file) = world(&root, "fixture");
    assert_eq!(context.owner_of(SERVICE_LSP), Some("lsp-registry"));
    let servers = lsp.servers().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id().as_str(), "fixture-lsp");
    assert_eq!(servers[0].language_id(), "rust");
    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "lsp-stdio"
            && row.kind == heycode_core::ContributionKind::ExternalProcess
            && row.name == "fixture-lsp"
    }));
    let position = LspPosition::new(0, 3);

    let definitions = lsp
        .definition(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].path(), &file);
    assert_eq!(
        definitions[0].range(),
        LspRange::new(LspPosition::new(2, 3), LspPosition::new(2, 7)).unwrap()
    );

    let references = lsp
        .references(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(references.len(), 2);
    assert!(references.iter().all(|location| location.path() == &file));

    let hover = lsp
        .hover(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hover.contents(), "```rust\nfn main()\n```");
    assert_eq!(hover.range().unwrap().start(), LspPosition::new(0, 0));

    let implementations = lsp
        .implementations(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(implementations.len(), 1);
    assert_eq!(implementations[0].range().start(), LspPosition::new(6, 1));

    let document_symbols = lsp
        .document_symbols(
            LspDocumentRequest::new(server_id(), file.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(document_symbols.len(), 2);
    assert_eq!(document_symbols[0].name(), "main");
    assert_eq!(document_symbols[1].name(), "unused");
    assert_eq!(document_symbols[1].container_name(), Some("main"));

    let workspace_symbols = lsp
        .workspace_symbols(
            LspWorkspaceSymbolRequest::new(server_id(), "main").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(workspace_symbols.len(), 1);
    assert_eq!(workspace_symbols[0].name(), "main");
    assert_eq!(workspace_symbols[0].path(), &file);
    assert_eq!(
        workspace_symbols[0].range().start(),
        LspPosition::new(0, 0),
        "an unresolved LSP 3.17 WorkspaceSymbol carries only its URI"
    );

    let incoming = lsp
        .incoming_calls(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].symbol().name(), "caller");
    assert_eq!(incoming[0].call_ranges().len(), 1);

    let outgoing = lsp
        .outgoing_calls(
            LspPositionRequest::new(server_id(), file.clone(), position),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].symbol().name(), "callee");

    let diagnostics = lsp
        .diagnostics(
            LspDocumentRequest::new(server_id(), file.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity(), LspDiagnosticSeverity::Warning);
    assert_eq!(diagnostics[0].message(), "unused variable fixture");
    assert_eq!(diagnostics[0].source(), Some("fixture-lsp"));

    wait_for(&root.join("lsp-ready")).await;
    context.shutdown();
    assert_eq!(
        lsp.servers().unwrap_err().code(),
        LspErrorCode::ServiceStopped
    );
    assert_eq!(
        lsp.definition(
            LspPositionRequest::new(server_id(), file, position),
            CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .code(),
        LspErrorCode::ServiceStopped
    );
    std::fs::write(root.join("release-descendant"), b"release").unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!root.join("lsp-descendant-survived").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn caller_cancellation_sends_cancel_request_without_detaching_the_server() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let (mut context, lsp, file) = world(&root, "cancel");
    let cancellation = CancellationToken::new();
    let task = {
        let lsp = lsp.clone();
        let cancellation = cancellation.clone();
        let file = file.clone();
        tokio::spawn(async move {
            lsp.definition(
                LspPositionRequest::new(server_id(), file, LspPosition::new(0, 0)),
                cancellation,
            )
            .await
        })
    };
    wait_for(&root.join("lsp-request")).await;
    cancellation.cancel();
    assert_eq!(
        task.await.unwrap().unwrap_err().code(),
        LspErrorCode::Cancelled
    );
    wait_for(&root.join("lsp-cancel")).await;
    let references = lsp
        .references(
            LspPositionRequest::new(server_id(), file, LspPosition::new(0, 0)),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(references.len(), 2, "cancelled request poisoned server");
    context.shutdown();
}

#[tokio::test]
async fn pushed_diagnostics_are_used_when_the_server_has_no_pull_provider() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { let unused = 1; }\n").unwrap();
    let (mut context, lsp, file) = world(&root, "push-diagnostics");
    let diagnostics = lsp
        .diagnostics(
            LspDocumentRequest::new(server_id(), file),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message(), "pushed diagnostic fixture");
    assert_eq!(diagnostics[0].source(), Some("fixture-lsp-push"));
    context.shutdown();
}

#[tokio::test]
async fn added_operations_reject_hostile_server_shapes_without_leaking_them() {
    for (mode, operation) in [("hostile-hover", "hover"), ("hostile-symbol", "symbol")] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let (mut context, lsp, file) = world(&root, mode);
        let error = if operation == "hover" {
            lsp.hover(
                LspPositionRequest::new(server_id(), file, LspPosition::new(0, 0)),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
        } else {
            lsp.workspace_symbols(
                LspWorkspaceSymbolRequest::new(server_id(), "hostile").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
        };
        assert_eq!(error.code(), LspErrorCode::Protocol);
        assert_eq!(error.to_string(), "LSP server protocol failed");
        context.shutdown();
    }
}

#[cfg(unix)]
#[test]
fn definitions_require_exact_interactive_argv_and_workspace_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let plain = ProcessSpec::new(std::env::current_exe().unwrap(), &root).unwrap();
    assert_eq!(
        LspServerDefinition::new(
            server_id(),
            "rust",
            ResolvedPath::new(&root).unwrap(),
            plain,
        )
        .unwrap_err()
        .code(),
        LspErrorCode::InvalidSpec
    );
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(
        LspServerDefinition::new(
            server_id(),
            "rust",
            ResolvedPath::new(&root).unwrap(),
            ProcessSpec::new(
                std::env::current_exe().unwrap(),
                elsewhere.path().canonicalize().unwrap(),
            )
            .unwrap()
            .with_interactive_stdio(),
        )
        .unwrap_err()
        .code(),
        LspErrorCode::InvalidSpec
    );
}

#[test]
fn lsp_helper_process() {
    if std::env::var(HELPER_ENV).ok().as_deref() != Some("1") {
        return;
    }
    run_helper();
}

fn run_helper() {
    let mode = std::env::var(MODE_ENV).unwrap();
    let ready = PathBuf::from(std::env::var_os(READY_ENV).unwrap());
    let request_seen = PathBuf::from(std::env::var_os(REQUEST_ENV).unwrap());
    let cancel_seen = PathBuf::from(std::env::var_os(CANCEL_ENV).unwrap());
    let release = PathBuf::from(std::env::var_os(RELEASE_ENV).unwrap());
    let survived = PathBuf::from(std::env::var_os(SURVIVED_ENV).unwrap());
    let mut descendant = spawn_descendant(&release, &survived);
    std::fs::write(&ready, b"ready").unwrap();

    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    while let Some(message) = read_message(&mut input) {
        let method = message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let id = message.get("id").cloned();
        match method {
            "initialize" => {
                let capabilities = if mode == "push-diagnostics" {
                    serde_json::json!({})
                } else {
                    serde_json::json!({
                        "diagnosticProvider":{
                            "interFileDependencies":false,
                            "workspaceDiagnostics":false
                        }
                    })
                };
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!({"capabilities":capabilities}),
                );
            }
            "initialized" => {}
            "textDocument/didOpen" => {
                if mode == "fixture" {
                    assert!(
                        message["params"]["textDocument"]["text"]
                            .as_str()
                            .unwrap()
                            .contains("unused")
                    );
                }
                if mode == "push-diagnostics" {
                    let uri = message["params"]["textDocument"]["uri"].clone();
                    write_message(
                        &mut output,
                        serde_json::json!({
                            "jsonrpc":"2.0",
                            "method":"textDocument/publishDiagnostics",
                            "params":{
                                "uri":uri,
                                "diagnostics":[{
                                    "range":range(0, 16, 0, 22),
                                    "severity":2,
                                    "source":"fixture-lsp-push",
                                    "message":"pushed diagnostic fixture"
                                }]
                            }
                        }),
                    );
                }
            }
            "textDocument/didChange" => {}
            "textDocument/definition" => {
                std::fs::write(&request_seen, b"definition").unwrap();
                if mode != "cancel" {
                    let uri = message["params"]["textDocument"]["uri"].clone();
                    write_response(
                        &mut output,
                        id.unwrap(),
                        serde_json::json!({
                            "uri":uri,
                            "range":range(2, 3, 2, 7)
                        }),
                    );
                }
            }
            "textDocument/references" => {
                let uri = message["params"]["textDocument"]["uri"].clone();
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([
                        {"uri":uri,"range":range(1, 0, 1, 4)},
                        {"uri":uri,"range":range(4, 2, 4, 6)}
                    ]),
                );
            }
            "textDocument/hover" => {
                let contents = if mode == "hostile-hover" {
                    "secret\u{1}body"
                } else {
                    "```rust\nfn main()\n```"
                };
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!({
                        "contents":{"kind":"markdown","value":contents},
                        "range":range(0, 0, 0, 4)
                    }),
                );
            }
            "textDocument/implementation" => {
                let uri = message["params"]["textDocument"]["uri"].clone();
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([{"uri":uri,"range":range(6, 1, 6, 5)}]),
                );
            }
            "textDocument/documentSymbol" => write_response(
                &mut output,
                id.unwrap(),
                serde_json::json!([{
                    "name":"main","detail":"fn main()","containerName":"","kind":12,
                    "range":range(0, 0, 0, 30),"selectionRange":range(0, 3, 0, 7),
                    "children":[{
                        "name":"unused","kind":13,"range":range(0, 16, 0, 22),
                        "selectionRange":range(0, 16, 0, 22)
                    }]
                }]),
            ),
            "workspace/symbol" => {
                assert_eq!(
                    message["params"]["query"],
                    if mode == "hostile-symbol" {
                        "hostile"
                    } else {
                        "main"
                    }
                );
                let uri = if mode == "hostile-symbol" {
                    outside_file_uri_from_ready_path(&ready)
                } else {
                    file_uri_from_ready_path(&ready)
                };
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([{
                        "name":"main","kind":12,"location":{"uri":uri}
                    }]),
                );
            }
            "textDocument/prepareCallHierarchy" => {
                let uri = message["params"]["textDocument"]["uri"].clone();
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([call_item("main", uri, 0)]),
                );
            }
            "callHierarchy/incomingCalls" => {
                let uri = message["params"]["item"]["uri"].clone();
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([{
                        "from":call_item("caller", uri, 3),"fromRanges":[range(3, 2, 3, 6)]
                    }]),
                );
            }
            "callHierarchy/outgoingCalls" => {
                let uri = message["params"]["item"]["uri"].clone();
                write_response(
                    &mut output,
                    id.unwrap(),
                    serde_json::json!([{
                        "to":call_item("callee", uri, 4),"fromRanges":[range(0, 8, 0, 12)]
                    }]),
                );
            }
            "textDocument/diagnostic" => write_response(
                &mut output,
                id.unwrap(),
                serde_json::json!({
                    "kind":"full",
                    "items":[{
                        "range":range(0, 16, 0, 22),
                        "severity":2,
                        "source":"fixture-lsp",
                        "message":"unused variable fixture"
                    }]
                }),
            ),
            "$/cancelRequest" => {
                std::fs::write(&cancel_seen, b"cancelled").unwrap();
            }
            _ => {
                if let Some(id) = id {
                    write_error(&mut output, id);
                }
            }
        }
    }
    if let Some(child) = descendant.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn range(
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
) -> serde_json::Value {
    serde_json::json!({
        "start":{"line":start_line,"character":start_character},
        "end":{"line":end_line,"character":end_character}
    })
}

fn call_item(name: &str, uri: serde_json::Value, line: u32) -> serde_json::Value {
    serde_json::json!({
        "name":name,"kind":12,"uri":uri,
        "range":range(line, 0, line, 8),"selectionRange":range(line, 0, line, 8)
    })
}

fn file_uri_from_ready_path(ready: &Path) -> serde_json::Value {
    let root = ready.parent().unwrap();
    serde_json::Value::String(
        url::Url::from_file_path(root.join("src/main.rs"))
            .unwrap()
            .to_string(),
    )
}

fn outside_file_uri_from_ready_path(ready: &Path) -> serde_json::Value {
    let outside = ready.parent().unwrap().parent().unwrap().join("outside.rs");
    serde_json::Value::String(url::Url::from_file_path(outside).unwrap().to_string())
}

fn read_message(input: &mut impl Read) -> Option<serde_json::Value> {
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if input.read(&mut byte).ok()? == 0 {
            return None;
        }
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).ok()?;
    let length = header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))?
        .trim()
        .parse::<usize>()
        .ok()?;
    let mut body = vec![0_u8; length];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_response(output: &mut impl Write, id: serde_json::Value, result: serde_json::Value) {
    write_message(
        output,
        serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),
    );
}

fn write_error(output: &mut impl Write, id: serde_json::Value) {
    write_message(
        output,
        serde_json::json!({
            "jsonrpc":"2.0","id":id,
            "error":{"code":-32601,"message":"method not found"}
        }),
    );
}

fn write_message(output: &mut impl Write, message: serde_json::Value) {
    let body = serde_json::to_vec(&message).unwrap();
    write!(output, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    output.write_all(&body).unwrap();
    output.flush().unwrap();
}

#[cfg(unix)]
fn spawn_descendant(release: &Path, survived: &Path) -> Option<std::process::Child> {
    let script = "while [ ! -e \"$1\" ]; do sleep 0.02; done; printf survived > \"$2\"";
    Some(
        Command::new("/bin/sh")
            .args(["-c", script, "heycode-lsp-descendant"])
            .arg(release)
            .arg(survived)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[cfg(not(unix))]
fn spawn_descendant(_release: &Path, _survived: &Path) -> Option<std::process::Child> {
    None
}
