//! `write` — create or overwrite a file, refusing blind overwrites.

use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Value, json};

use crate::builtins::{arg_str, filesystem_error, resolve_path};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) fn tool(filesystem: heycode_exec::FileSystemService) -> Arc<dyn Tool> {
    Arc::new(WriteTool { filesystem })
}

struct WriteTool {
    filesystem: heycode_exec::FileSystemService,
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "write".to_owned(),
        description: "Create a new file (parents included). Default mode=create refuses existing files. Prefer edit or multi_edit for existing files. Full replacement requires mode=replace and expected_revision from Read, and still refuses stale/unobserved files. Returns a short receipt, not the whole file."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                "content": {"type": "string", "maxLength":262144, "description": "Full text to write; at most 256 KiB. Prefer targeted edits for existing files."},
                "mode": {"type":"string","enum":["create","replace"],"description":"Defaults to create. Replace requires a current revision."},
                "expected_revision": {"type":"string","pattern":"^[0-9a-f]{64}$","description":"Required for replace: revision from read."}
            },
            "required": ["path", "content"]
        }),
    }
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus { configured: Some(true), detail: "Filesystem service is bound; paths, sandbox, permissions and operation preconditions are checked per invocation.".into() }
    }

    fn rebind_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(tool(filesystem.clone()))
    }

    fn spec(&self) -> ToolSpec {
        spec()
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let raw = arg_str(&args, "path")?;
        let content = arg_str(&args, "content")?;
        if content.len() > 262144 {
            return Err(ToolError::new(
                "Write content exceeds 256 KiB; use smaller targeted edits.",
            ));
        }
        let mode = args
            .get("mode")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| ToolError::new("mode must be create or replace"))
            })
            .transpose()?
            .unwrap_or("create");
        let path = resolve_path(&self.filesystem, cx, raw)?;
        let spec = heycode_exec::WriteFileSpec::new(path.clone(), content)
            .map_err(|error| filesystem_error(&error, "write", raw, Some(path.as_path())))?;
        let checked = match mode {
            "create" if args.get("expected_revision").is_none() => {
                heycode_exec::CheckedWriteSpec::create(spec)
            }
            "replace" => heycode_exec::CheckedWriteSpec::replace(
                spec,
                arg_str(&args, "expected_revision")?.to_owned(),
            )
            .map_err(|_| {
                ToolError::new("Replace requires the expected_revision token returned by read")
            })?,
            "create" => {
                return Err(ToolError::new(
                    "expected_revision is only used with mode=replace",
                ));
            }
            _ => return Err(ToolError::new("mode must be create or replace")),
        };
        let output = self.filesystem.write_checked(checked, cx.cancellation.clone()).await.map_err(|error| match error.code() {
            heycode_exec::FileSystemErrorCode::ChangedAtCommit if mode == "create" => ToolError::new(format!("{raw} already exists or changed before creation. Use edit/multi_edit, or explicitly use mode=replace with a current read revision.")),
            heycode_exec::FileSystemErrorCode::NotObserved => ToolError::new(format!("Read {raw} before overwriting.")),
            heycode_exec::FileSystemErrorCode::StaleObservation => ToolError::new(format!("{raw} changed since the supplied revision; read it again before replacing.")),
            _ => filesystem_error(&error,"write",raw,Some(path.as_path())),
        })?;
        Ok(
            json!({"message":if output.changed { format!("Wrote {} line{} to {raw}",content.lines().count(), if content.lines().count() == 1 { "" } else { "s" }) } else { format!("{raw} already has the requested content; unchanged") },
            "lines":content.lines().count(),"bytes":output.bytes,"revision":output.revision,"changed":output.changed}),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn run_write(dir: &Path, args: Value) -> Result<Value, ToolError> {
        tool(crate::builtins::test_filesystem(dir))
            .run(
                args,
                &ToolCtx {
                    cwd: dir.to_path_buf(),
                    ..Default::default()
                },
            )
            .await
    }

    #[tokio::test]
    async fn creates_file_and_parents_and_reports_line_count() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_write(
            dir.path(),
            json!({"path": "nested/dir/f.txt", "content": "a\nb\n"}),
        )
        .await
        .unwrap();
        assert_eq!(out["message"], json!("Wrote 2 lines to nested/dir/f.txt"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nested/dir/f.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn refuses_overwrite_of_unobserved_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "precious").unwrap();
        let err = run_write(dir.path(), json!({"path": "f.txt", "content": "clobbered"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "precious"
        );
    }

    #[tokio::test]
    async fn overwrites_after_the_path_was_observed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "old").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        let read = crate::builtins::read::read_page(
            &crate::ToolsConfig::default(),
            &filesystem,
            &json!({"path":"f.txt"}),
            &ToolCtx::default().with_cwd(dir.path().into()),
        )
        .await
        .unwrap();
        let out = tool(filesystem)
            .run(
                json!({"path": "f.txt", "content": "new\n", "mode":"replace", "expected_revision":read["revision"]}),
                &ToolCtx {
                    cwd: dir.path().to_path_buf(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out["message"], json!("Wrote 1 line to f.txt"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
    }

    #[test]
    fn spec_requires_both_arguments() {
        let s = spec();
        let required: Vec<&str> = s.parameters["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, vec!["path", "content"]);
    }
}
