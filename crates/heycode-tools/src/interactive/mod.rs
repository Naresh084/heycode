//! Provider-independent browser, local artifacts and notebook editing.
//! The plugin mounts ordinary tools into the shared approval/guard execution pipeline.
mod artifact;
mod browser;
mod computer;
mod notebook;
mod send_user_file;
mod speech;

use crate::{
    PendingRichToolResult, PendingToolMedia, PendingToolResultBlock, Tool, ToolCtx, ToolError,
    ToolOutput, ToolRegistry,
};
pub use browser::BrowserConfig;
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use sha2::{Digest, Sha256};
pub use speech::{LocalSpeech, SpeechCommandConfig, SpeechTranscript};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn prefix(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn file_error(error: heycode_exec::FileSystemError) -> ToolError {
    ToolError::new(format!(
        "Local file operation failed: {error}. Re-read changed files and use an allowed workspace path."
    ))
}

/// Even a helper that closes stdout early remains owned until actual process settlement.
async fn settle_helper(
    process: heycode_exec::ManagedProcess,
    operation: CancellationToken,
    closed: CancellationToken,
    caller: CancellationToken,
) -> Result<heycode_exec::ProcessExit, ToolError> {
    let wait = process.wait();
    tokio::pin!(wait);
    tokio::select! {
        biased;
        () = closed.cancelled() => {},
        () = caller.cancelled() => {},
        result = &mut wait => return result.map_err(|_| ToolError::new("Helper process failed during settlement.")),
        () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {},
    }
    operation.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), wait).await;
    Err(ToolError::new(
        "Helper cancelled or did not exit after closing output; process ownership retired.",
    ))
}
fn image_output(value: serde_json::Value, bytes: Vec<u8>) -> Result<ToolOutput, ToolError> {
    let error = |_| ToolError::new("Image could not be admitted as a bounded rich tool result.");
    let annotations =
        heycode_core::ToolResultAnnotations::new(Vec::new(), None, None, Default::default())
            .map_err(error)?;
    let metadata = heycode_core::ToolResultBlockMetadata::new(annotations, Default::default())
        .map_err(error)?;
    let media = PendingToolMedia::new(
        Some(
            heycode_core::AttachmentMediaType::new("image/png")
                .map_err(|_| ToolError::new("Invalid PNG media type."))?,
        ),
        bytes,
    )
    .map_err(|_| ToolError::new("Image exceeds rich tool limit."))?;
    let rich = PendingRichToolResult::new(
        vec![PendingToolResultBlock::Image { media, metadata }],
        heycode_core::ToolStructuredContent::Absent,
        heycode_core::ToolResultSchemaCheck::NoSchema,
        Default::default(),
    )
    .map_err(|_| ToolError::new("Image result validation failed."))?;
    Ok(ToolOutput::rich(value, rich))
}

// Workspace rebinding must never fall back to a captured parent executor. Retain the
// advertised schema while making unsupported scoped capabilities explicitly unavailable.
fn workspace_unavailable(tool: &dyn Tool) -> Arc<dyn Tool> {
    struct Unavailable {
        spec: heycode_core::ToolSpec,
        boundary: Option<heycode_core::UntrustedContentBoundary>,
    }
    #[async_trait::async_trait]
    impl Tool for Unavailable {
        fn spec(&self) -> heycode_core::ToolSpec {
            self.spec.clone()
        }
        fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
            self.boundary
        }
        async fn run(
            &self,
            _: serde_json::Value,
            _: &ToolCtx,
        ) -> Result<serde_json::Value, ToolError> {
            Err(ToolError::new(
                "Interactive tool cannot bind the provided workspace services; parent execution authority is not available.",
            ))
        }
    }
    Arc::new(Unavailable {
        spec: tool.spec(),
        boundary: tool.untrusted_content(),
    })
}

struct OwnedTool {
    inner: Arc<dyn Tool>,
    closed: CancellationToken,
}
#[async_trait::async_trait]
impl Tool for OwnedTool {
    fn rebind_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            inner: self
                .inner
                .rebind_workspace(filesystem, shell)
                .unwrap_or_else(|| workspace_unavailable(self.inner.as_ref())),
            // Parent plugin retirement still closes descendants. Child tool drop does not
            // cancel this shared token or retire a parent's browser/process state.
            closed: self.closed.clone(),
        }))
    }
    fn effect(&self) -> crate::ToolEffect {
        self.inner.effect()
    }
    fn supports_background(&self) -> bool {
        self.inner.supports_background()
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        self.inner.spec()
    }
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        self.inner.untrusted_content()
    }
    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        if self.closed.is_cancelled() || cx.cancellation.is_cancelled() {
            return Err(ToolError::new(
                "Interactive tools stopped or call cancelled.",
            ));
        }
        self.inner.run(args, cx).await
    }
    async fn run_output(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        if self.closed.is_cancelled() || cx.cancellation.is_cancelled() {
            return Err(ToolError::new(
                "Interactive tools stopped or call cancelled.",
            ));
        }
        self.inner.run_output(args, cx).await
    }
}

/// Default model tools with optional browser installation. No subprocess starts at composition.
/// Notebook/artifact operations need no optional runtime. Browser setup is reported by status.
#[must_use]
pub fn interactive_tools_plugin(config: Option<BrowserConfig>) -> Box<dyn Plugin> {
    interactive_tools_plugin_with_speech(config, None)
}

/// Compose the optional local STT command together with the ordinary interactive tools.
#[must_use]
pub fn interactive_tools_plugin_with_speech(
    config: Option<BrowserConfig>,
    speech: Option<SpeechCommandConfig>,
) -> Box<dyn Plugin> {
    struct Interactive {
        config: Option<BrowserConfig>,
        speech: Option<SpeechCommandConfig>,
    }
    impl Plugin for Interactive {
        fn name(&self) -> &'static str {
            "interactive-tools"
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
                "notebook_read",
                "notebook_edit",
                "artifact",
                "SendUserFile",
                "transcribe_audio",
                "computer",
                "browser",
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
                heycode_exec::SERVICE_FILESYSTEM,
                heycode_exec::SERVICE_SUBPROCESS,
                heycode_web::SERVICE_WEB,
            ]
        }
        fn apply(&self, ctx: &mut Context) -> CoreResult<()> {
            let registry = get::<ToolRegistry>(ctx, crate::SERVICE_TOOLS)?;
            let filesystem =
                get::<heycode_exec::FileSystemService>(ctx, heycode_exec::SERVICE_FILESYSTEM)?;
            let subprocess =
                get::<heycode_exec::SubprocessService>(ctx, heycode_exec::SERVICE_SUBPROCESS)?;
            let web = get::<heycode_web::WebRegistry>(ctx, heycode_web::SERVICE_WEB)?;
            let closed = CancellationToken::new();
            let mut tools = notebook::tools(filesystem.clone());
            tools.push(Arc::new(artifact::Artifacts::new(filesystem.clone())));
            tools.push(Arc::new(send_user_file::SendUserFile::new(
                filesystem.clone(),
            )));
            tools.push(Arc::new(speech::Speech {
                config: self.speech.clone(),
                filesystem: filesystem.clone(),
                subprocess: subprocess.clone(),
                closed: closed.clone(),
            }));
            tools.push(Arc::new(computer::Computer {
                filesystem: filesystem.clone(),
                subprocess: subprocess.clone(),
                closed: closed.clone(),
            }));
            tools.push(Arc::new(browser::Browser::new(
                self.config.clone(),
                filesystem,
                subprocess,
                web,
                closed.clone(),
            )));
            let mut registrations = Vec::new();
            for inner in tools {
                registrations.push(
                    registry
                        .register_owned(Arc::new(OwnedTool {
                            inner,
                            closed: closed.clone(),
                        }))
                        .map_err(|e| CoreError::other(e.to_string()))?,
                );
            }
            ctx.effect(move || {
                closed.cancel();
                drop(registrations);
            });
            Ok(())
        }
    }
    Box::new(Interactive { config, speech })
}
fn get<T: Send + Sync + 'static>(
    ctx: &Context,
    key: heycode_core::ServiceKey,
) -> CoreResult<Arc<T>> {
    ctx.get::<T>(key)
        .ok_or_else(|| CoreError::MissingService(key.to_string()))
}

#[cfg(test)]
mod tests;
