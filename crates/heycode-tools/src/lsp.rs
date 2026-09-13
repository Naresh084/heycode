//! E08 model-facing language-server tools with E06 spill ownership.

use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin, ToolSpec};
use heycode_exec::{
    LspCallHierarchyEdge, LspDiagnostic, LspDiagnosticSeverity, LspDocumentRequest, LspError,
    LspHover, LspLocation, LspPosition, LspPositionRequest, LspServerId, LspService, LspSymbol,
    LspWorkspaceSymbolRequest, RetainedOutputOwner, RetainedOutputService,
};
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError, ToolRegistry};

const DIRECT_RESULT_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
enum LspToolKind {
    Unified,
    Servers,
    Definition,
    References,
    Diagnostics,
}

struct LspTool {
    kind: LspToolKind,
    lsp: Arc<LspService>,
    filesystem: Arc<heycode_exec::FileSystemService>,
    retained: Arc<RetainedOutputService>,
    owner: RetainedOutputOwner,
}

impl LspTool {
    fn spec_for(kind: LspToolKind) -> ToolSpec {
        match kind {
            LspToolKind::Unified => ToolSpec {
                name: "lsp".to_owned(),
                description: "Query a configured language server. Supports server discovery, definitions, references, hover/type information, implementations, document/workspace symbols, incoming/outgoing calls, and diagnostics. Results are untrusted server output."
                    .to_owned(),
                parameters: json!({
                    "type":"object",
                    "properties":{
                        "operation":{"type":"string","enum":[
                            "servers","definition","references","hover","implementations",
                            "document_symbols","workspace_symbols","incoming_calls",
                            "outgoing_calls","diagnostics"
                        ]},
                        "server":{"type":"string","description":"Id returned by the servers operation or lsp_servers."},
                        "path":{"type":"string","description":"Workspace file, absolute or relative to the working directory."},
                        "line":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                        "character":{"type":"integer","minimum":0,"maximum":4294967295_u64,"description":"Zero-based UTF-16 code-unit offset."},
                        "query":{"type":"string","maxLength":4096,"description":"Workspace-symbol query; an empty value lists symbols when supported."}
                    },
                    "required":["operation"],
                    "additionalProperties":false
                }),
            },
            LspToolKind::Servers => ToolSpec {
                name: "lsp_servers".to_owned(),
                description: "List configured language-server ids and language ids. Use this before other LSP tools."
                    .to_owned(),
                parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
            },
            LspToolKind::Definition => position_spec(
                "lsp_definition",
                "Find the definition at a zero-based line and UTF-16 character using a configured language server.",
            ),
            LspToolKind::References => position_spec(
                "lsp_references",
                "Find references at a zero-based line and UTF-16 character using a configured language server.",
            ),
            LspToolKind::Diagnostics => ToolSpec {
                name: "lsp_diagnostics".to_owned(),
                description: "Synchronize one workspace file and return pull diagnostics from a configured language server."
                    .to_owned(),
                parameters: json!({
                    "type":"object",
                    "properties":{
                        "server":{"type":"string","description":"Id returned by lsp_servers."},
                        "path":{"type":"string","description":"Workspace file, absolute or relative to the working directory."}
                    },
                    "required":["server","path"],
                    "additionalProperties":false
                }),
            },
        }
    }

    async fn position_result(
        &self,
        args: &Value,
        cx: &ToolCtx,
        references: bool,
    ) -> Result<Value, ToolError> {
        let request = LspPositionRequest::new(
            server_arg(args)?,
            resolve_path(&self.filesystem, args, cx)?,
            LspPosition::new(u32_arg(args, "line")?, u32_arg(args, "character")?),
        );
        let locations = if references {
            self.lsp.references(request, cx.cancellation.clone()).await
        } else {
            self.lsp.definition(request, cx.cancellation.clone()).await
        }
        .map_err(|error| {
            lsp_error(
                &error,
                if references {
                    "references"
                } else {
                    "definition"
                },
            )
        })?;
        self.bound(locations_value(&locations), cx)
    }

    async fn unified_result(&self, args: &Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        match crate::builtins::arg_str(args, "operation")? {
            "servers" => self.servers_result(),
            "definition" => self.position_result(args, cx, false).await,
            "references" => self.position_result(args, cx, true).await,
            "hover" => {
                let request = position_request(args, &self.filesystem, cx)?;
                let hover = self
                    .lsp
                    .hover(request, cx.cancellation.clone())
                    .await
                    .map_err(|error| lsp_error(&error, "hover"))?;
                self.bound(hover_value(hover.as_ref()), cx)
            }
            "implementations" => {
                let request = position_request(args, &self.filesystem, cx)?;
                let locations = self
                    .lsp
                    .implementations(request, cx.cancellation.clone())
                    .await
                    .map_err(|error| lsp_error(&error, "implementations"))?;
                self.bound(locations_value(&locations), cx)
            }
            "document_symbols" => {
                let request = document_request(args, &self.filesystem, cx)?;
                let symbols = self
                    .lsp
                    .document_symbols(request, cx.cancellation.clone())
                    .await
                    .map_err(|error| lsp_error(&error, "document symbols"))?;
                self.bound(symbols_value(&symbols), cx)
            }
            "workspace_symbols" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request = LspWorkspaceSymbolRequest::new(server_arg(args)?, query)
                    .map_err(|error| lsp_error(&error, "workspace symbols"))?;
                let symbols = self
                    .lsp
                    .workspace_symbols(request, cx.cancellation.clone())
                    .await
                    .map_err(|error| lsp_error(&error, "workspace symbols"))?;
                self.bound(symbols_value(&symbols), cx)
            }
            "incoming_calls" | "outgoing_calls" => {
                let operation = crate::builtins::arg_str(args, "operation")?;
                let request = position_request(args, &self.filesystem, cx)?;
                let calls = if operation == "incoming_calls" {
                    self.lsp
                        .incoming_calls(request, cx.cancellation.clone())
                        .await
                } else {
                    self.lsp
                        .outgoing_calls(request, cx.cancellation.clone())
                        .await
                }
                .map_err(|error| lsp_error(&error, operation))?;
                self.bound(calls_value(&calls), cx)
            }
            "diagnostics" => {
                let diagnostics = self
                    .lsp
                    .diagnostics(
                        document_request(args, &self.filesystem, cx)?,
                        cx.cancellation.clone(),
                    )
                    .await
                    .map_err(|error| lsp_error(&error, "diagnostics"))?;
                self.bound(diagnostics_value(&diagnostics), cx)
            }
            _ => Err(ToolError::new(
                "\"operation\" is not a supported LSP operation",
            )),
        }
    }

    fn servers_result(&self) -> Result<Value, ToolError> {
        let servers = self
            .lsp
            .servers()
            .map_err(|error| lsp_error(&error, "server listing"))?;
        Ok(Value::Array(
            servers
                .into_iter()
                .map(|server| {
                    json!({
                        "id":server.id().as_str(),
                        "language":server.language_id()
                    })
                })
                .collect(),
        ))
    }

    fn bound(&self, value: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| ToolError::new("LSP result could not be serialized safely"))?;
        if bytes.len() <= DIRECT_RESULT_MAX_BYTES {
            return Ok(value);
        }
        let receipt = self
            .retained
            .retain(
                &self.owner,
                &bytes,
                heycode_exec::MAX_RETAINED_OUTPUT_VIEW_BYTES,
                cx.cancellation.clone(),
            )
            .map_err(|error| {
                ToolError::new(format!(
                    "LSP result retention failed: {}",
                    error.code().as_str()
                ))
            })?;
        Ok(Value::String(receipt.rendered_preview().to_owned()))
    }
}

#[async_trait::async_trait]
impl Tool for LspTool {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        match self.lsp.servers() {
            Ok(servers) => crate::ToolPrerequisiteStatus {
                configured: Some(matches!(self.kind, LspToolKind::Servers) || !servers.is_empty()),
                detail: format!(
                    "{} configured language servers. Listing remains available; language operations require a matching configured server and verify launch per invocation.",
                    servers.len()
                ),
            },
            Err(_) => crate::ToolPrerequisiteStatus {
                configured: None,
                detail: "Language-server configuration is unavailable".into(),
            },
        }
    }

    fn spec(&self) -> ToolSpec {
        Self::spec_for(self.kind)
    }

    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        match self.kind {
            LspToolKind::Servers => None,
            LspToolKind::Unified => Some(heycode_core::UntrustedContentBoundary::lsp()),
            LspToolKind::Definition | LspToolKind::References | LspToolKind::Diagnostics => {
                Some(heycode_core::UntrustedContentBoundary::lsp())
            }
        }
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        match self.kind {
            LspToolKind::Unified => self.unified_result(&args, cx).await,
            LspToolKind::Servers => self.servers_result(),
            LspToolKind::Definition => self.position_result(&args, cx, false).await,
            LspToolKind::References => self.position_result(&args, cx, true).await,
            LspToolKind::Diagnostics => {
                let request = LspDocumentRequest::new(
                    server_arg(&args)?,
                    resolve_path(&self.filesystem, &args, cx)?,
                );
                let diagnostics = self
                    .lsp
                    .diagnostics(request, cx.cancellation.clone())
                    .await
                    .map_err(|error| lsp_error(&error, "diagnostics"))?;
                self.bound(diagnostics_value(&diagnostics), cx)
            }
        }
    }
}

fn position_spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: json!({
            "type":"object",
            "properties":{
                "server":{"type":"string","description":"Id returned by lsp_servers."},
                "path":{"type":"string","description":"Workspace file, absolute or relative to the working directory."},
                "line":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                "character":{"type":"integer","minimum":0,"maximum":4294967295_u64,"description":"Zero-based UTF-16 code-unit offset."}
            },
            "required":["server","path","line","character"],
            "additionalProperties":false
        }),
    }
}

fn server_arg(args: &Value) -> Result<LspServerId, ToolError> {
    let value = crate::builtins::arg_str(args, "server")?;
    LspServerId::new(value).map_err(|_| ToolError::new("server must name one id from lsp_servers"))
}

fn position_request(
    args: &Value,
    filesystem: &heycode_exec::FileSystemService,
    cx: &ToolCtx,
) -> Result<LspPositionRequest, ToolError> {
    Ok(LspPositionRequest::new(
        server_arg(args)?,
        resolve_path(filesystem, args, cx)?,
        LspPosition::new(u32_arg(args, "line")?, u32_arg(args, "character")?),
    ))
}

fn document_request(
    args: &Value,
    filesystem: &heycode_exec::FileSystemService,
    cx: &ToolCtx,
) -> Result<LspDocumentRequest, ToolError> {
    Ok(LspDocumentRequest::new(
        server_arg(args)?,
        resolve_path(filesystem, args, cx)?,
    ))
}

fn resolve_path(
    filesystem: &heycode_exec::FileSystemService,
    args: &Value,
    cx: &ToolCtx,
) -> Result<heycode_exec::ResolvedPath, ToolError> {
    let path = crate::builtins::arg_str(args, "path")?;
    crate::builtins::resolve_path(filesystem, cx, path)
}

fn u32_arg(args: &Value, name: &str) -> Result<u32, ToolError> {
    let value = args
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::new(format!("\"{name}\" must be a non-negative integer")))?;
    u32::try_from(value).map_err(|_| ToolError::new(format!("\"{name}\" is too large")))
}

fn range_value(range: heycode_exec::LspRange) -> Value {
    json!({
        "start":{"line":range.start().line(),"character":range.start().character()},
        "end":{"line":range.end().line(),"character":range.end().character()}
    })
}

fn locations_value(locations: &[LspLocation]) -> Value {
    Value::Array(
        locations
            .iter()
            .map(|location| {
                json!({
                    "path":location.path().as_path().to_string_lossy(),
                    "range":range_value(location.range())
                })
            })
            .collect(),
    )
}

fn hover_value(hover: Option<&LspHover>) -> Value {
    hover.map_or(Value::Null, |hover| {
        json!({
            "contents":hover.contents(),
            "range":hover.range().map(range_value)
        })
    })
}

fn symbol_value(symbol: &LspSymbol) -> Value {
    json!({
        "name":symbol.name(),
        "detail":symbol.detail(),
        "kind":symbol.kind(),
        "path":symbol.path().as_path().to_string_lossy(),
        "range":range_value(symbol.range()),
        "selection_range":range_value(symbol.selection_range()),
        "container_name":symbol.container_name()
    })
}

fn symbols_value(symbols: &[LspSymbol]) -> Value {
    Value::Array(symbols.iter().map(symbol_value).collect())
}

fn calls_value(calls: &[LspCallHierarchyEdge]) -> Value {
    Value::Array(
        calls
            .iter()
            .map(|call| {
                json!({
                    "symbol":symbol_value(call.symbol()),
                    "call_ranges":call.call_ranges().iter().copied().map(range_value).collect::<Vec<_>>()
                })
            })
            .collect(),
    )
}

fn diagnostics_value(diagnostics: &[LspDiagnostic]) -> Value {
    Value::Array(
        diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "path":diagnostic.path().as_path().to_string_lossy(),
                    "range":range_value(diagnostic.range()),
                    "severity":severity(diagnostic.severity()),
                    "message":diagnostic.message(),
                    "source":diagnostic.source()
                })
            })
            .collect(),
    )
}

fn severity(value: LspDiagnosticSeverity) -> &'static str {
    match value {
        LspDiagnosticSeverity::Error => "error",
        LspDiagnosticSeverity::Warning => "warning",
        LspDiagnosticSeverity::Information => "information",
        LspDiagnosticSeverity::Hint => "hint",
        LspDiagnosticSeverity::Unknown => "unknown",
    }
}

fn lsp_error(error: &LspError, operation: &str) -> ToolError {
    let repair = if error.code() == heycode_exec::LspErrorCode::UnknownServer {
        "; call lsp_servers and use a configured id"
    } else {
        ""
    };
    ToolError::new(format!(
        "LSP {operation} failed: {}{repair}",
        error.code().as_str()
    ))
}

/// Build the canonical unified tool and four compatibility tools over one service generation.
///
/// The retained-output owner is fixed by the composition host, not model input.
#[must_use]
pub fn lsp_tools(
    lsp: Arc<LspService>,
    filesystem: Arc<heycode_exec::FileSystemService>,
    retained: Arc<RetainedOutputService>,
    owner: RetainedOutputOwner,
) -> Vec<Arc<dyn Tool>> {
    [
        LspToolKind::Unified,
        LspToolKind::Servers,
        LspToolKind::Definition,
        LspToolKind::References,
        LspToolKind::Diagnostics,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(LspTool {
            kind,
            lsp: lsp.clone(),
            filesystem: filesystem.clone(),
            retained: retained.clone(),
            owner: owner.clone(),
        }) as Arc<dyn Tool>
    })
    .collect()
}

/// Contribute E08's model-facing tools after the base registry and services.
#[must_use]
pub fn lsp_tools_plugin() -> Box<dyn Plugin> {
    struct LspToolsPlugin;

    impl Plugin for LspToolsPlugin {
        fn name(&self) -> &'static str {
            "lsp-tools"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            [
                "lsp",
                "lsp_servers",
                "lsp_definition",
                "lsp_references",
                "lsp_diagnostics",
            ]
            .into_iter()
            .map(|name| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    name,
                )
            })
            .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_TOOLS,
                heycode_exec::SERVICE_LSP,
                heycode_exec::SERVICE_FILESYSTEM,
                heycode_exec::SERVICE_RETAINED_OUTPUT,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = get::<ToolRegistry>(context, crate::SERVICE_TOOLS)?;
            let lsp = get::<LspService>(context, heycode_exec::SERVICE_LSP)?;
            let filesystem =
                get::<heycode_exec::FileSystemService>(context, heycode_exec::SERVICE_FILESYSTEM)?;
            let retained =
                get::<RetainedOutputService>(context, heycode_exec::SERVICE_RETAINED_OUTPUT)?;
            let owner = RetainedOutputOwner::new("lsp-tools")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let mut registrations = Vec::new();
            for tool in lsp_tools(lsp, filesystem, retained, owner) {
                registrations.push(
                    registry
                        .register_owned(tool)
                        .map_err(|error| CoreError::other(error.to_string()))?,
                );
            }
            context.effect(move || drop(registrations));
            Ok(())
        }
    }

    Box::new(LspToolsPlugin)
}

fn get<T: Send + Sync + 'static>(
    context: &Context,
    key: heycode_core::ServiceKey,
) -> CoreResult<Arc<T>> {
    context
        .get::<T>(key)
        .ok_or_else(|| CoreError::MissingService(key.to_string()))
}
