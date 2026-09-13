//! One bounded batch of independent paginated reads, with per-file errors.
use super::read::{optional_usize, read_page, read_properties};
use crate::config::ToolsConfig;
use crate::{Tool, ToolCtx, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn tool(
    cfg: Arc<ToolsConfig>,
    filesystem: heycode_exec::FileSystemService,
) -> Arc<dyn Tool> {
    Arc::new(ReadMany { cfg, filesystem })
}
struct ReadMany {
    cfg: Arc<ToolsConfig>,
    filesystem: heycode_exec::FileSystemService,
}

#[async_trait::async_trait]
impl Tool for ReadMany {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "read_many".into(),
            description: "Read up to 16 files with independent offsets and revisions in one call. Each file defaults to 200 lines; the batch shares a 1,000-line and 32-KiB content budget. Returns per-file pages or errors and lists files deferred when the budget is exhausted. Narrow offsets before increasing limits. Does not mutate files.".into(),
            parameters: json!({"type":"object","properties":{
                "files":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","properties":read_properties(),"required":["path"],"additionalProperties":false}},
                "max_total_lines":{"type":"integer","minimum":1,"maximum":1000,"description":"Shared line ceiling; default 1000."},
                "max_total_bytes":{"type":"integer","minimum":4,"maximum":65536,"description":"Shared retained text-byte ceiling; default 32768."}
            },"required":["files"],"additionalProperties":false}),
        }
    }
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus {
            configured: Some(true),
            detail:
                "Filesystem service is bound; each path retains its own policy and revision checks."
                    .into(),
        }
    }
    fn rebind_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        _: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(tool(self.cfg.clone(), filesystem.clone()))
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let files = args
            .get("files")
            .and_then(Value::as_array)
            .filter(|files| (1..=16).contains(&files.len()))
            .ok_or_else(|| ToolError::new("files must contain between 1 and 16 read requests"))?;
        let mut remaining_lines = optional_usize(&args, "max_total_lines", 1000, 1000)?;
        let mut remaining_bytes = optional_usize(&args, "max_total_bytes", 32768, 65536)?;
        let mut results = Vec::new();
        for request in files {
            if cx.cancellation.is_cancelled() {
                return Err(ToolError::new("Batch read cancelled"));
            }
            let raw = super::arg_str(request, "path")?;
            if raw.len() > 4096 {
                return Err(ToolError::new("path exceeds 4096 bytes"));
            }
            if remaining_lines == 0 || remaining_bytes < 4 {
                results.push(json!({"path":raw,"status":"deferred","reason":"Batch output budget exhausted; read this file in a follow-up call"}));
                continue;
            }
            let mut bounded = request.clone();
            // Validate per-file values before narrowing them; malformed input
            // never silently falls back to a large default read.
            let limit = optional_usize(request, "limit", super::read::DEFAULT_READ_LINES, 2000)?
                .min(remaining_lines);
            let bytes = optional_usize(request, "max_bytes", 16384, 262144)?.min(remaining_bytes);
            bounded["limit"] = json!(limit);
            bounded["max_bytes"] = json!(bytes);
            match read_page(&self.cfg, &self.filesystem, &bounded, cx).await {
                Ok(mut page) => {
                    remaining_lines = remaining_lines.saturating_sub(
                        page["lines_returned"].as_u64().unwrap_or_default() as usize,
                    );
                    // Count exact retained bytes, including CRLF and partial lines.
                    let used = page["bytes_returned"].as_u64().unwrap_or_default() as usize;
                    remaining_bytes = remaining_bytes.saturating_sub(used);
                    page["status"] = json!("read");
                    results.push(page);
                }
                Err(error) => {
                    if cx.cancellation.is_cancelled() {
                        return Err(ToolError::new("Batch read cancelled"));
                    }
                    results.push(json!({"path":raw,"status":"error","error":error.message}));
                }
            }
        }
        Ok(
            json!({"files":results,"remaining_line_budget":remaining_lines,"remaining_byte_budget":remaining_bytes}),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn batch_shares_budget_retains_errors_and_does_not_read_deferred_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(name), "one\ntwo\nthree\n").unwrap();
        }
        let filesystem = crate::builtins::test_filesystem(dir.path());
        let observations = filesystem.observations();
        let tool = tool(Arc::new(ToolsConfig::default()), filesystem);
        let result = tool.run(json!({"files":[{"path":"missing"},{"path":"a"},{"path":"b"},{"path":"c"}],"max_total_lines":4}), &ToolCtx::default().with_cwd(dir.path().into())).await.unwrap();
        assert_eq!(result["files"][0]["status"], "error");
        assert_eq!(result["files"][1]["lines_returned"], 3);
        assert_eq!(result["files"][2]["lines_returned"], 1);
        assert_eq!(result["files"][2]["next_offset"], 2);
        assert_eq!(result["files"][3]["status"], "deferred");
        assert!(!observations.contains(&dir.path().join("c")));
    }
}
