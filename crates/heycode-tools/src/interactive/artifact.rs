//! Bounded local artifact handles; files are re-authorized and rehashed on preview.
use super::{digest, file_error, image_output, prefix};
use crate::builtins::{arg_str, resolve_path};
use crate::{Tool, ToolCtx, ToolError, ToolOutput};
use heycode_core::ToolSpec;
use heycode_exec::{FileSystemService, ReadFileSpec};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub(super) struct Artifacts {
    filesystem: Arc<FileSystemService>,
    entries: Mutex<BTreeMap<String, (String, String)>>,
}
impl Artifacts {
    pub(super) fn new(filesystem: Arc<FileSystemService>) -> Self {
        Self {
            filesystem,
            entries: Mutex::new(BTreeMap::new()),
        }
    }
    async fn execute(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let action = arg_str(&args, "action")?;
        if action == "list" {
            let entries = self
                .entries
                .lock()
                .map_err(|_| ToolError::new("Artifact registry unavailable."))?;
            return Ok(ToolOutput::plain(
                json!({"artifacts":entries.iter().map(|(id,(path,revision))| json!({"id":id,"path":path,"revision":revision})).collect::<Vec<_>>() }),
            ));
        }
        let (raw, expected) = match action {
            "register" => (arg_str(&args, "path")?.to_owned(), None),
            "preview" => {
                let entries = self
                    .entries
                    .lock()
                    .map_err(|_| ToolError::new("Artifact registry unavailable."))?;
                let (path, revision) = entries
                    .get(arg_str(&args, "id")?)
                    .ok_or_else(|| ToolError::new("Unknown artifact; register it first."))?;
                (path.clone(), Some(revision.clone()))
            }
            "remove" => {
                self.entries
                    .lock()
                    .map_err(|_| ToolError::new("Artifact registry unavailable."))?
                    .remove(arg_str(&args, "id")?);
                return Ok(ToolOutput::plain(
                    json!({"removed":true,"file_deleted":false}),
                ));
            }
            _ => {
                return Err(ToolError::new(
                    "action must be register, preview, list or remove.",
                ));
            }
        };
        let path = resolve_path(&self.filesystem, cx, &raw)?;
        let read = self
            .filesystem
            .read(
                ReadFileSpec::new_binary(path.clone(), 8 * 1024 * 1024).map_err(file_error)?,
                cx.cancellation.clone(),
            )
            .await
            .map_err(file_error)?;
        if read.truncated() {
            return Err(ToolError::new("Artifact exceeds 8 MiB."));
        }
        let revision = digest(read.bytes());
        if expected.is_some_and(|v| v != revision) {
            return Err(ToolError::new(
                "Artifact changed since registration; register its new revision first.",
            ));
        }
        let path_text = path.as_path().to_string_lossy().to_string();
        let id = digest(path_text.as_bytes());
        if action == "register" {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ToolError::new("Artifact registry unavailable."))?;
            if entries.len() >= 64 && !entries.contains_key(&id) {
                return Err(ToolError::new(
                    "Artifact limit reached (64); remove an old handle.",
                ));
            }
            entries.insert(id.clone(), (path_text.clone(), revision.clone()));
        }
        let is_png = read.bytes().starts_with(b"\x89PNG\r\n\x1a\n");
        let mut value = json!({"id":id,"path":path_text,"revision":revision,"bytes":read.bytes().len(),"preview_kind":if is_png{"image/png"}else{"text_or_download"},"image_input_requires_vision":true});
        if action == "preview" {
            if is_png && args["include_image"] == true {
                return image_output(value, read.bytes().to_vec());
            }
            if let Ok(text) = std::str::from_utf8(read.bytes()) {
                value["text"] = json!(prefix(text, 16384));
                value["truncated"] = json!(text.len() > 16384);
            } else {
                value["message"] = json!(
                    "Binary artifact. Open the local path for human preview; PNG model preview requires include_image=true and a vision model."
                );
            }
        }
        Ok(ToolOutput::plain(value))
    }
}
#[async_trait::async_trait]
impl Tool for Artifacts {
    fn rebind_workspace(
        &self,
        filesystem: &FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self::new(Arc::new(filesystem.clone()))))
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name:"artifact".into(),description:"Register a generated local file, list/remove its handle, or preview its exact registered revision. Text/HTML source previews are inert and bounded. PNG include_image=true explicitly requires model vision; default returns metadata/text only. Files stay local; nothing is uploaded or deployed by this tool.".into(),parameters:json!({"type":"object","properties":{"action":{"enum":["register","preview","list","remove"]},"path":{"type":"string"},"id":{"type":"string"},"include_image":{"type":"boolean"}},"required":["action"],"additionalProperties":false}) }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let (value, _, _) = self.execute(args, cx).await?.into_parts();
        Ok(value)
    }
    async fn run_output(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.execute(args, cx).await
    }
}
