//! Fixed native macOS adapter, separate from provider-hosted computer-use protocols.
use super::{file_error, image_output};
use crate::builtins::{arg_str, resolve_path};
use crate::{Tool, ToolCtx, ToolError, ToolOutput};
use base64::Engine as _;
use heycode_exec::{FileSystemService, SubprocessService};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct Computer {
    pub(super) filesystem: Arc<FileSystemService>,
    pub(super) subprocess: Arc<SubprocessService>,
    pub(super) closed: CancellationToken,
}
fn validate(args: &Value) -> Result<&str, ToolError> {
    let action = arg_str(args, "action")?;
    if !matches!(
        action,
        "status" | "inspect" | "screenshot" | "click" | "type" | "key"
    ) {
        return Err(ToolError::new("Unsupported computer action."));
    }
    if action != "status" || args.get("bundle_id").is_some() {
        let bundle = arg_str(args, "bundle_id")?;
        if bundle.is_empty()
            || bundle.len() > 256
            || !bundle
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return Err(ToolError::new("Pass an exact application bundle_id."));
        }
    }
    if matches!(action, "click" | "type" | "key") {
        let revision = arg_str(args, "expected_revision")?;
        if revision.len() != 64 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ToolError::new(
                "Pass expected_revision from a fresh computer inspect or screenshot.",
            ));
        }
    }
    if matches!(action, "click" | "type") && args.get("element").is_some() {
        let element = arg_str(args, "element")?;
        if element.len() > 128 || !element.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return Err(ToolError::new(
                "Pass an accessibility element ref from computer inspect.",
            ));
        }
    }
    if action == "click"
        && args.get("element").is_none()
        && (!args["x"].as_f64().is_some_and(|n| n.is_finite())
            || !args["y"].as_f64().is_some_and(|n| n.is_finite()))
    {
        return Err(ToolError::new(
            "Coordinate clicks require x/y from the target window screenshot.",
        ));
    }
    if action == "type" && arg_str(args, "text")?.len() > 16384 {
        return Err(ToolError::new("Computer text exceeds 16 KiB."));
    }
    if action == "key" {
        if !matches!(
            arg_str(args, "key")?,
            "return"
                | "tab"
                | "escape"
                | "backspace"
                | "delete"
                | "left"
                | "right"
                | "down"
                | "up"
                | "a"
                | "c"
                | "v"
                | "x"
                | "z"
                | "s"
        ) {
            return Err(ToolError::new("Unsupported key."));
        }
        if let Some(modifiers) = args.get("modifiers") {
            let modifiers = modifiers
                .as_array()
                .ok_or_else(|| ToolError::new("modifiers must be an array."))?;
            if modifiers.len() > 4
                || modifiers.iter().any(|m| {
                    !matches!(m.as_str(), Some("command" | "shift" | "option" | "control"))
                })
            {
                return Err(ToolError::new("Unsupported key modifiers."));
            }
        }
    }
    if args.to_string().len() > 65536 {
        return Err(ToolError::new("Computer request exceeds 64 KiB."));
    }
    Ok(action)
}
impl Computer {
    async fn execute(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let action = validate(&args)?;
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (action, cx, &self.subprocess, &self.closed, &self.filesystem);
            return Ok(ToolOutput::plain(
                json!({"platform":std::env::consts::OS,"supported":false,"reason":"Native computer adapter currently supports macOS only."}),
            ));
        }
        #[cfg(target_os = "macos")]
        {
            let path = if action == "screenshot" {
                Some(resolve_path(&self.filesystem, cx, arg_str(&args, "path")?)?)
            } else {
                None
            };
            let cwd = resolve_path(&self.filesystem, cx, ".")?;
            let spec = heycode_exec::ProcessSpec::new("/usr/bin/swift", cwd.as_path())
                .and_then(|s| {
                    s.with_args([
                        "-swift-version",
                        "5",
                        "-e",
                        include_str!("computer-macos.swift"),
                    ])
                })
                .map_err(|_| ToolError::new("Native helper configuration invalid."))?
                .with_interactive_stdio();
            let mut value =
                run_helper(&self.subprocess, spec, &args, cx, self.closed.clone()).await?;
            if let Some(code) = value["error"].as_str() {
                return Err(platform_error(code));
            }
            if let Some(path) = path {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(arg_str(&value, "png")?)
                    .map_err(|_| ToolError::new("Native helper returned invalid PNG."))?;
                if bytes.len() > 8 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(ToolError::new("Native screenshot is invalid or oversized."));
                }
                if let Some(object) = value.as_object_mut() {
                    object.remove("png");
                }
                self.filesystem
                    .write(
                        heycode_exec::WriteFileSpec::new(path.clone(), &bytes)
                            .map_err(file_error)?,
                        cx.cancellation.clone(),
                    )
                    .await
                    .map_err(file_error)?;
                value["path"] = json!(path.as_path());
                value["image_input_requires_vision"] = json!(true);
                if args["include_image"] == true {
                    return image_output(value, bytes);
                }
            }
            Ok(ToolOutput::plain(value))
        }
    }
}
#[cfg(target_os = "macos")]
fn platform_error(code: &str) -> ToolError {
    ToolError::new(match code {
        "accessibility_permission_required" => {
            "macOS Accessibility permission is required for the launching app/helper. Enable it in System Settings > Privacy & Security > Accessibility, then retry."
        }
        "screen_recording_permission_required" => {
            "macOS Screen & System Audio Recording permission is required for the launching app/helper. Enable it in System Settings > Privacy & Security, then retry."
        }
        "target_not_running" => {
            "Target application is not running. Launch the intended app and use its exact bundle_id."
        }
        "stale_revision" => {
            "Application state or selected window changed; inspect or capture again before input."
        }
        "element_not_pressable" => {
            "This accessibility element does not support pressing; inspect for its actionable child."
        }
        "element_not_editable" => {
            "This accessibility element does not support setting a text value."
        }
        "target_window_not_on_screen" => {
            "Target window is off screen. Make the app window visible on the active desktop, capture it again, then retry input."
        }
        "outside_target_window" => "Click coordinates must be inside the captured target window.",
        "target_window_unavailable" => "Target application has no capturable normal window.",
        "macos_14_required" => "Native screenshot capture requires macOS 14 or later.",
        _ => {
            "Native computer operation failed. Check target application, current element references and OS permissions."
        }
    })
}
#[cfg(target_os = "macos")]
async fn run_helper(
    subprocess: &SubprocessService,
    spec: heycode_exec::ProcessSpec,
    args: &Value,
    cx: &ToolCtx,
    closed: CancellationToken,
) -> Result<Value, ToolError> {
    use heycode_exec::ProcessOutputChunk;
    let operation = cx.cancellation.child_token();
    let raw=subprocess.spawn_interactive_raw(spec,operation.clone()).await.map_err(|_|ToolError::new("Native helper could not start. Install Apple Command Line Tools (xcode-select --install) and check sandbox policy."))?;
    let (process, mut input, mut output) = raw.into_raw_parts();
    let work = async {
        input
            .write(args.to_string().as_bytes())
            .await
            .map_err(|_| ToolError::new("Native helper input failed."))?;
        input
            .finish()
            .await
            .map_err(|_| ToolError::new("Native helper input failed."))?;
        let mut bytes = Vec::new();
        loop {
            match output
                .read_chunk(cx.cancellation.clone())
                .await
                .map_err(|_| ToolError::new("Native helper output failed."))?
            {
                ProcessOutputChunk::Eof => break,
                ProcessOutputChunk::Data(chunk) => {
                    if bytes.len() + chunk.len() > 12 * 1024 * 1024 {
                        return Err(ToolError::new("Native helper output exceeds 12 MiB."));
                    }
                    bytes.extend(chunk);
                }
            }
        }
        serde_json::from_slice::<Value>(&bytes).map_err(|_| ToolError::new(
            "Native helper returned invalid output. Check Apple Command Line Tools and sandbox policy."
        ))
    };
    let result = tokio::select! {biased;()=closed.cancelled()=>Err(ToolError::new("Native computer adapter stopped.")),()=cx.cancellation.cancelled()=>Err(ToolError::new("Native computer action cancelled.")),result=tokio::time::timeout(std::time::Duration::from_secs(45),work)=>result.unwrap_or_else(|_|Err(ToolError::new("Native helper timed out.")))};
    drop(output);
    match result {
        Err(error) => {
            process
                .kill()
                .await
                .map_err(|_| ToolError::new("Native helper cleanup failed."))?;
            Err(error)
        }
        Ok(value) => {
            let exit =
                super::settle_helper(process, operation, closed, cx.cancellation.clone()).await?;
            if !exit.is_success() {
                return Err(ToolError::new(
                    "Native helper exited unsuccessfully. Check Apple Command Line Tools.",
                ));
            }
            Ok(value)
        }
    }
}
#[async_trait::async_trait]
impl Tool for Computer {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus {
            configured: if cfg!(target_os = "macos") { None } else { Some(false) },
            detail: "Requires macOS, Apple Command Line Tools and operation-specific OS permissions. Use computer status to inspect permissions; this inventory does not probe or request them.".into(),
        }
    }
    fn rebind_workspace(
        &self,
        filesystem: &FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(match shell.subprocess() {
            Some(subprocess) => Arc::new(Self {
                filesystem: Arc::new(filesystem.clone()),
                subprocess: Arc::new(subprocess),
                closed: self.closed.child_token(),
            }),
            None => super::workspace_unavailable(self),
        })
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec{name:"computer".into(),description:"Native macOS app-targeted accessibility and screenshots, independent of model provider. status checks OS permissions without prompting/capture. inspect(bundle_id) returns text element refs and revision. click/type/key require expected_revision; click(element) uses AXPress and type(element) replaces its value. Without element, input requires an on-screen window; click(x,y)/type use a screenshot geometry revision and target-PID events; key also posts to that PID. screenshot captures only the target app window to path; include_image=true requires vision. Requires macOS permissions and Apple Command Line Tools; never opens a microphone.".into(),parameters:json!({"type":"object","properties":{"action":{"enum":["status","inspect","screenshot","click","type","key"]},"bundle_id":{"type":"string"},"expected_revision":{"type":"string"},"element":{"type":"string"},"x":{"type":"number"},"y":{"type":"number"},"text":{"type":"string","maxLength":16384},"key":{"enum":["return","tab","escape","backspace","delete","left","right","down","up","a","c","v","x","z","s"]},"modifiers":{"type":"array","items":{"enum":["command","shift","option","control"]},"maxItems":4},"path":{"type":"string"},"include_image":{"type":"boolean"}},"required":["action"],"additionalProperties":false})}
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let (value, _, _) = self.execute(args, cx).await?.into_parts();
        Ok(value)
    }
    async fn run_output(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.execute(args, cx).await
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn actions_require_target_and_conflict_revision() {
        assert!(validate(&json!({"action":"status"})).is_ok());
        assert!(validate(&json!({"action":"inspect","bundle_id":"org.heycode.fixture"})).is_ok());
        for value in [
            json!({"action":"eval","script":"danger"}),
            json!({"action":"type","bundle_id":"org.heycode.fixture","element":"0.1","text":"hello"}),
            json!({"action":"key","bundle_id":"org.heycode.fixture","expected_revision":"a".repeat(64),"key":"unsupported"}),
            json!({"action":"inspect","bundle_id":"../app"}),
        ] {
            assert!(validate(&value).is_err());
        }
    }
}
