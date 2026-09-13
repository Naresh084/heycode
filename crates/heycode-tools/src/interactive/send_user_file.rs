//! Bounded local-conversation file delivery through the Agent-owned rich-result path.

use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::Arc;

use heycode_core::{
    ToolResultAnnotations, ToolResultAudience, ToolResultBlockMetadata, ToolResultSchemaCheck,
    ToolSpec, ToolStructuredContent,
};
use heycode_exec::{FileSystemService, PathRequest, ReadFileSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    PendingRichToolResult, PendingToolMedia, PendingToolResultBlock, Tool, ToolCtx, ToolError,
    ToolOutput,
};

const MAX_FILES: usize = 8;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_CAPTION_BYTES: usize = 512;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendUserFileArgs {
    files: Vec<String>,
    caption: Option<String>,
    status: DeliveryStatus,
    display: Option<DeliveryDisplay>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryStatus {
    Normal,
    Proactive,
}

impl DeliveryStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Proactive => "proactive",
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryDisplay {
    Render,
    Attach,
}

impl DeliveryDisplay {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Render => "render",
            Self::Attach => "attach",
        }
    }
}

struct PreparedFile {
    path: String,
    revision: String,
    content_id: String,
    resource_uri: String,
    bytes: Vec<u8>,
}

/// Exact-reference local file-delivery tool. Registration remains owned by the
/// interactive plugin composition root.
pub(super) struct SendUserFile {
    filesystem: Arc<FileSystemService>,
}

impl SendUserFile {
    /// Bind one workspace-scoped filesystem generation.
    pub(super) fn new(filesystem: Arc<FileSystemService>) -> Self {
        Self { filesystem }
    }

    async fn execute(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: SendUserFileArgs = serde_json::from_value(args).map_err(|_| {
            ToolError::new(
                "SendUserFile arguments are invalid; pass files, status, and only the documented optional fields.",
            )
        })?;
        validate_args(&args)?;
        if cx.cancellation.is_cancelled() {
            return Err(ToolError::new("Local file delivery was cancelled."));
        }

        let cwd = self
            .filesystem
            .resolve(
                PathRequest::new(&cx.cwd, ".")
                    .map_err(|_| ToolError::new("The active workspace directory is invalid."))?,
            )
            .map_err(|_| ToolError::new("The active workspace directory is unavailable."))?;
        let root = active_root(&self.filesystem, cwd.as_path()).ok_or_else(|| {
            ToolError::new("The active directory is outside the allowed workspace roots.")
        })?;

        let metadata = user_block_metadata()?;
        let mut total_bytes = 0_usize;
        let mut seen = BTreeSet::new();
        let mut prepared = Vec::with_capacity(args.files.len());
        for (index, raw_path) in args.files.iter().enumerate() {
            if cx.cancellation.is_cancelled() {
                return Err(ToolError::new("Local file delivery was cancelled."));
            }
            let request = PathRequest::new(cwd.as_path(), raw_path)
                .map_err(|_| ToolError::new("A delivery path is invalid."))?;
            let resolved = self
                .filesystem
                .resolve(request)
                .map_err(super::file_error)?;
            if !resolved.as_path().starts_with(&root) {
                return Err(ToolError::new(
                    "Every delivered file must belong to the active workspace root.",
                ));
            }
            let path = normalized_relative_path(resolved.as_path(), &root)?;
            if !seen.insert(path.clone()) {
                return Err(ToolError::new(
                    "The files list contains the same workspace file more than once.",
                ));
            }
            let read = self
                .filesystem
                .read(
                    ReadFileSpec::new_binary(resolved, MAX_FILE_BYTES)
                        .map_err(super::file_error)?,
                    cx.cancellation.clone(),
                )
                .await
                .map_err(super::file_error)?;
            if read.truncated() {
                return Err(ToolError::new(
                    "A delivered file exceeds the 8 MiB per-file limit.",
                ));
            }
            if read.bytes().is_empty() {
                return Err(ToolError::new("Empty files cannot be delivered."));
            }
            total_bytes = total_bytes
                .checked_add(read.bytes().len())
                .ok_or_else(|| ToolError::new("The delivery byte total is invalid."))?;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err(ToolError::new(
                    "Delivered files exceed the 16 MiB total limit.",
                ));
            }
            let revision = format!("{:x}", Sha256::digest(read.bytes()));
            prepared.push(PreparedFile {
                path,
                content_id: format!("sha256-{revision}"),
                revision,
                resource_uri: format!("heycode://local-user-file/{}", index + 1),
                bytes: read.bytes().to_vec(),
            });
        }
        if cx.cancellation.is_cancelled() {
            return Err(ToolError::new("Local file delivery was cancelled."));
        }

        let count = prepared.len();
        let summary = format!(
            "Prepared {count} local file{} for this conversation. No remote or mobile delivery occurred.",
            if count == 1 { "" } else { "s" }
        );
        let files = prepared
            .iter()
            .enumerate()
            .map(|(index, file)| {
                json!({
                    "index": index + 1,
                    "path": file.path,
                    "revision": file.revision,
                    "content_id": file.content_id,
                    "byte_len": file.bytes.len(),
                    "resource_uri": file.resource_uri,
                })
            })
            .collect::<Vec<_>>();
        let mut receipt = json!({
            "schema_version": 1,
            "delivery": "local_conversation",
            "local_only": true,
            "remote_sent": false,
            "status": args.status.as_str(),
            "display": args.display.unwrap_or(DeliveryDisplay::Attach).as_str(),
            "files": files,
        });
        if let Some(caption) = args.caption {
            receipt["caption"] = Value::String(caption);
        }

        let mut blocks = Vec::with_capacity(count + 1);
        blocks.push(PendingToolResultBlock::Text {
            text: summary,
            metadata: metadata.clone(),
        });
        for (index, file) in prepared.into_iter().enumerate() {
            let mut extensions = serde_json::Map::new();
            extensions.insert("heycode_user_file_index".to_owned(), json!(index + 1));
            blocks.push(PendingToolResultBlock::EmbeddedBlob {
                uri: file.resource_uri,
                media: PendingToolMedia::new(None, file.bytes).map_err(|_| {
                    ToolError::new("A delivered file exceeds the rich-result media limit.")
                })?,
                resource_extensions: extensions,
                metadata: metadata.clone(),
            });
        }
        let rich = PendingRichToolResult::new(
            blocks,
            ToolStructuredContent::Present(receipt.clone()),
            ToolResultSchemaCheck::NoSchema,
            serde_json::Map::new(),
        )
        .map_err(|_| ToolError::new("The local delivery receipt exceeds its safety bounds."))?;
        Ok(ToolOutput::rich(receipt, rich))
    }
}

#[async_trait::async_trait]
impl Tool for SendUserFile {
    fn rebind_workspace(
        &self,
        filesystem: &FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self::new(Arc::new(filesystem.clone()))))
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "SendUserFile".to_owned(),
            description: "Make one or more bounded files from the active workspace available to the user in this local conversation. This retains exact bytes through the local attachment store; it does not send to a phone, account, cloud service, or remote client. `status` and `display` are presentation intent only.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_FILES,
                        "items": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES}
                    },
                    "caption": {"type": "string", "minLength": 1, "maxLength": MAX_CAPTION_BYTES},
                    "status": {"type": "string", "enum": ["normal", "proactive"]},
                    "display": {"type": "string", "enum": ["render", "attach"]}
                },
                "required": ["files", "status"],
                "additionalProperties": false
            }),
        }
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let (value, _, _) = self.execute(args, cx).await?.into_parts();
        Ok(value)
    }

    async fn run_output(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.execute(args, cx).await
    }
}

fn validate_args(args: &SendUserFileArgs) -> Result<(), ToolError> {
    if args.files.is_empty() || args.files.len() > MAX_FILES {
        return Err(ToolError::new(
            "files must contain between one and eight workspace paths.",
        ));
    }
    if args.files.iter().any(|path| {
        path.is_empty() || path.len() > MAX_PATH_BYTES || path.chars().any(char::is_control)
    }) {
        return Err(ToolError::new(
            "Every delivery path must be non-empty, at most 4096 bytes, and contain no control characters.",
        ));
    }
    if args.caption.as_ref().is_some_and(|caption| {
        caption.is_empty()
            || caption.len() > MAX_CAPTION_BYTES
            || caption.chars().any(char::is_control)
    }) {
        return Err(ToolError::new(
            "caption must be non-empty, at most 512 bytes, and contain no control characters.",
        ));
    }
    Ok(())
}

fn active_root(filesystem: &FileSystemService, cwd: &Path) -> Option<std::path::PathBuf> {
    filesystem
        .policy()
        .roots()
        .iter()
        .filter(|root| cwd.starts_with(root.path()))
        .max_by_key(|root| root.path().components().count())
        .map(|root| root.path().to_path_buf())
}

fn normalized_relative_path(path: &Path, root: &Path) -> Result<String, ToolError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ToolError::new("A delivery path is outside the active workspace root."))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(ToolError::new(
                "A delivery path cannot be normalized safely.",
            ));
        };
        let component = component
            .to_str()
            .filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
            .ok_or_else(|| ToolError::new("A delivery path cannot be represented safely."))?;
        parts.push(component);
    }
    let normalized = parts.join("/");
    if normalized.is_empty() || normalized.len() > MAX_PATH_BYTES {
        return Err(ToolError::new("A normalized delivery path is invalid."));
    }
    Ok(normalized)
}

fn user_block_metadata() -> Result<ToolResultBlockMetadata, ToolError> {
    let annotations = ToolResultAnnotations::new(
        vec![ToolResultAudience::User],
        None,
        None,
        serde_json::Map::new(),
    )
    .map_err(|_| ToolError::new("Local delivery metadata is invalid."))?;
    ToolResultBlockMetadata::new(annotations, serde_json::Map::new())
        .map_err(|_| ToolError::new("Local delivery metadata is invalid."))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_exec::{FileSystemPolicy, FileSystemRoot, FileSystemRootAccess};

    fn filesystem(roots: &[&Path]) -> Arc<FileSystemService> {
        let policy = FileSystemPolicy::new(
            roots
                .iter()
                .map(|root| FileSystemRoot::new(root, FileSystemRootAccess::ReadWrite).unwrap()),
        )
        .unwrap();
        Arc::new(FileSystemService::local(policy).unwrap())
    }

    fn cx(root: &Path) -> ToolCtx {
        ToolCtx::default().with_cwd(root.to_path_buf())
    }

    #[test]
    fn schema_matches_the_reference_shape_without_claiming_remote_delivery() {
        let root = tempfile::tempdir().unwrap();
        let tool = SendUserFile::new(filesystem(&[root.path()]));
        let spec = tool.spec();

        assert_eq!(spec.name, "SendUserFile");
        assert_eq!(spec.parameters["required"], json!(["files", "status"]));
        assert_eq!(spec.parameters["properties"]["files"]["maxItems"], 8);
        assert_eq!(
            spec.parameters["properties"]["status"]["enum"],
            json!(["normal", "proactive"])
        );
        assert_eq!(
            spec.parameters["properties"]["display"]["enum"],
            json!(["render", "attach"])
        );
        assert_eq!(spec.parameters["additionalProperties"], false);
        assert!(spec.description.contains("local conversation"));
        assert!(spec.description.contains("does not send"));
    }

    #[tokio::test]
    async fn ordered_exact_bytes_become_user_only_embedded_resources() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("out")).unwrap();
        std::fs::write(root.path().join("out/report.txt"), b"report\n").unwrap();
        std::fs::write(root.path().join("opaque.bin"), [0_u8, 1, 2, 3]).unwrap();
        let tool = SendUserFile::new(filesystem(&[root.path()]));

        let output = tool
            .execute(
                json!({
                    "files": ["out/report.txt", "opaque.bin"],
                    "caption": "Requested outputs",
                    "status": "proactive",
                    "display": "render"
                }),
                &cx(root.path()),
            )
            .await
            .unwrap();
        let (value, pending, is_error) = output.into_parts();
        assert!(!is_error);
        assert_eq!(value["delivery"], "local_conversation");
        assert!(value.get("state").is_none());
        assert_eq!(value["local_only"], true);
        assert_eq!(value["remote_sent"], false);
        assert_eq!(value["status"], "proactive");
        assert_eq!(value["display"], "render");
        assert_eq!(value["caption"], "Requested outputs");
        assert_eq!(value["files"][0]["path"], "out/report.txt");
        assert_eq!(value["files"][1]["path"], "opaque.bin");
        assert_eq!(
            value["files"][0]["revision"],
            format!("{:x}", Sha256::digest(b"report\n"))
        );
        assert_eq!(
            value["files"][1]["content_id"],
            format!("sha256-{:x}", Sha256::digest([0_u8, 1, 2, 3]))
        );

        let (blocks, structured, schema, extensions) = pending.unwrap().into_parts();
        assert_eq!(blocks.len(), 3);
        assert!(matches!(schema, ToolResultSchemaCheck::NoSchema));
        assert!(extensions.is_empty());
        assert_eq!(structured.value(), Some(&value));
        let PendingToolResultBlock::Text { text, metadata } = &blocks[0] else {
            panic!("first block must be the local-only receipt");
        };
        assert!(text.contains("No remote or mobile delivery occurred"));
        assert_eq!(metadata.annotations.audience(), &[ToolResultAudience::User]);
        let PendingToolResultBlock::EmbeddedBlob {
            uri,
            media,
            resource_extensions,
            ..
        } = &blocks[2]
        else {
            panic!("opaque file must remain an embedded blob");
        };
        assert_eq!(uri, "heycode://local-user-file/2");
        assert_eq!(media.declared_media_type(), None);
        assert_eq!(media.bytes(), [0_u8, 1, 2, 3]);
        assert_eq!(resource_extensions["heycode_user_file_index"], 2);
    }

    #[tokio::test]
    async fn omitted_display_defaults_to_an_attachment_card_intent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one.txt"), b"one").unwrap();
        let tool = SendUserFile::new(filesystem(&[root.path()]));

        let output = tool
            .execute(
                json!({"files":["one.txt"],"status":"normal"}),
                &cx(root.path()),
            )
            .await
            .unwrap();
        let (value, _, _) = output.into_parts();
        assert_eq!(value["display"], "attach");
        assert!(value.get("caption").is_none());
    }

    #[tokio::test]
    async fn active_root_prevents_cross_root_delivery() {
        let active = tempfile::tempdir().unwrap();
        let sibling = tempfile::tempdir().unwrap();
        std::fs::write(active.path().join("active.txt"), b"active").unwrap();
        std::fs::write(sibling.path().join("sibling.txt"), b"sibling").unwrap();
        let tool = SendUserFile::new(filesystem(&[active.path(), sibling.path()]));

        let error = tool
            .execute(
                json!({
                    "files":[sibling.path().join("sibling.txt").to_string_lossy()],
                    "status":"normal"
                }),
                &cx(active.path()),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("active workspace root"));
    }

    #[tokio::test]
    async fn workspace_rebind_drops_parent_file_authority() {
        let parent = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        std::fs::write(parent.path().join("parent.txt"), b"parent").unwrap();
        std::fs::write(child.path().join("child.txt"), b"child").unwrap();
        let tool = SendUserFile::new(filesystem(&[parent.path()]));
        let child_filesystem = filesystem(&[child.path()]);
        let child_shell = heycode_exec::ShellService::local(
            heycode_exec::LocalShellConfig::platform(
                child.path().to_path_buf(),
                std::time::Duration::from_secs(1),
            )
            .unwrap(),
        );
        let rebound = tool
            .rebind_workspace(&child_filesystem, &child_shell)
            .unwrap();

        let parent_error = rebound
            .run_output(
                json!({
                    "files":[parent.path().join("parent.txt").to_string_lossy()],
                    "status":"normal"
                }),
                &cx(child.path()),
            )
            .await
            .unwrap_err();
        assert!(parent_error.message.contains("Local file operation failed"));
        assert!(
            !parent_error
                .message
                .contains(&parent.path().display().to_string())
        );

        let child_output = rebound
            .run_output(
                json!({"files":["child.txt"],"status":"normal"}),
                &cx(child.path()),
            )
            .await
            .unwrap();
        let (receipt, pending, is_error) = child_output.into_parts();
        assert!(!is_error);
        assert!(pending.is_some());
        assert_eq!(receipt["files"][0]["path"], "child.txt");
    }

    #[tokio::test]
    async fn duplicates_oversize_empty_and_cancelled_inputs_fail_before_a_receipt() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one.txt"), b"one").unwrap();
        std::fs::write(root.path().join("empty.txt"), b"").unwrap();
        std::fs::write(
            root.path().join("large.bin"),
            vec![7_u8; MAX_FILE_BYTES + 1],
        )
        .unwrap();
        let tool = SendUserFile::new(filesystem(&[root.path()]));

        let duplicate = tool
            .execute(
                json!({"files":["one.txt","one.txt"],"status":"normal"}),
                &cx(root.path()),
            )
            .await
            .unwrap_err();
        assert!(duplicate.message.contains("more than once"));

        let empty = tool
            .execute(
                json!({"files":["empty.txt"],"status":"normal"}),
                &cx(root.path()),
            )
            .await
            .unwrap_err();
        assert!(empty.message.contains("Empty files"));

        let large = tool
            .execute(
                json!({"files":["large.bin"],"status":"normal"}),
                &cx(root.path()),
            )
            .await
            .unwrap_err();
        assert!(large.message.contains("8 MiB"));

        let cancelled_cx = cx(root.path());
        cancelled_cx.cancellation.cancel();
        let cancelled = tool
            .execute(
                json!({"files":["one.txt"],"status":"normal"}),
                &cancelled_cx,
            )
            .await
            .unwrap_err();
        assert!(cancelled.message.contains("cancelled"));
    }

    #[tokio::test]
    async fn aggregate_media_limit_is_enforced_before_rich_result_publication() {
        let root = tempfile::tempdir().unwrap();
        for name in ["one.bin", "two.bin", "three.bin"] {
            std::fs::write(root.path().join(name), vec![7_u8; 6 * 1024 * 1024]).unwrap();
        }
        let tool = SendUserFile::new(filesystem(&[root.path()]));

        let error = tool
            .execute(
                json!({
                    "files":["one.bin","two.bin","three.bin"],
                    "status":"normal"
                }),
                &cx(root.path()),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("16 MiB total limit"));
    }

    #[tokio::test]
    async fn malformed_and_control_bearing_inputs_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("one.txt"), b"one").unwrap();
        let tool = SendUserFile::new(filesystem(&[root.path()]));

        for args in [
            json!({"files":[],"status":"normal"}),
            json!({"files":["one.txt"],"status":"unexpected"}),
            json!({"files":["one.txt"],"status":"normal","display":"inline"}),
            json!({"files":["one.txt"],"status":"normal","extra":true}),
            json!({"files":["one\n.txt"],"status":"normal"}),
            json!({"files":["one.txt"],"status":"normal","caption":"bad\ncaption"}),
        ] {
            assert!(tool.execute(args, &cx(root.path())).await.is_err());
        }
    }
}
