//! `read` — numbered text-file viewer with size caps and binary detection.

use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Value, json};

use crate::builtins::{arg_str, filesystem_error, resolve_path};
use crate::config::ToolsConfig;
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) fn tool(
    cfg: Arc<ToolsConfig>,
    filesystem: heycode_exec::FileSystemService,
) -> Arc<dyn Tool> {
    Arc::new(ReadTool { cfg, filesystem })
}

struct ReadTool {
    cfg: Arc<ToolsConfig>,
    filesystem: heycode_exec::FileSystemService,
}

pub(crate) const DEFAULT_READ_LINES: usize = 200;
pub(crate) const DEFAULT_READ_BYTES: usize = 16 * 1024;

pub(crate) fn read_properties() -> Value {
    json!({
        "path": {"type":"string","maxLength":4096,"description":"File path, absolute or relative to the working directory."},
        "offset": {"type":"integer","minimum":1,"description":"One-based first line; defaults to 1. Use next_offset to continue."},
        "limit": {"type":"integer","minimum":1,"maximum":2000,"description":"Maximum lines, default 200. Request more only when needed."},
        "max_bytes": {"type":"integer","minimum":4,"maximum":262144,"description":"Maximum retained text bytes, default 16384; independent of the line limit."},
        "byte_offset": {"type":"integer","minimum":0,"description":"Exact next_byte_offset for continuing a byte-capped long line. Do not combine with offset."},
        "expected_revision": {"type":"string","pattern":"^[0-9a-f]{64}$","description":"Revision from a previous read; refuses a page from a changed file."}
    })
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "read".into(),
        description: "Read a bounded text page: 200 lines and 16 KiB by default. Returns numbered content, total bytes, total lines (null beyond the bounded 64 MiB counting scan), revision and continuation. Use offset/limit for later lines; use next_byte_offset for long-line continuation. Pass expected_revision when continuing or preparing an edit. Prefer grep to locate relevant sections instead of reading an entire large file.".into(),
        parameters: json!({"type":"object","properties":read_properties(),"required":["path"],"additionalProperties":false}),
    }
}

pub(crate) fn optional_usize(
    args: &Value,
    name: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, ToolError> {
    let Some(value) = args.get(name) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0 && *value <= maximum)
        .ok_or_else(|| ToolError::new(format!("{name} must be an integer from 1 to {maximum}")))?;
    Ok(value)
}

pub(crate) async fn read_page(
    cfg: &ToolsConfig,
    filesystem: &heycode_exec::FileSystemService,
    args: &Value,
    cx: &ToolCtx,
) -> Result<Value, ToolError> {
    let raw = arg_str(args, "path")?;
    if raw.len() > 4096 {
        return Err(ToolError::new("path exceeds 4096 bytes"));
    }
    let offset = optional_usize(args, "offset", 1, usize::MAX)?;
    let limit = optional_usize(args, "limit", DEFAULT_READ_LINES, 2000)?.min(cfg.read_max_lines);
    let max_bytes =
        optional_usize(args, "max_bytes", DEFAULT_READ_BYTES, 262144)?.min(cfg.read_max_bytes);
    let byte_offset = args
        .get("byte_offset")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| ToolError::new("byte_offset must be a nonnegative integer"))
        })
        .transpose()?;
    if byte_offset.is_some() && args.get("offset").is_some() {
        return Err(ToolError::new("Use offset or byte_offset, not both"));
    }
    let path = resolve_path(filesystem, cx, raw)?;
    let mut spec = heycode_exec::ReadFileSpec::new(path.clone(), max_bytes)
        .and_then(|spec| spec.with_window(offset, limit, byte_offset))
        .map_err(|error| filesystem_error(&error, "read", raw, Some(path.as_path())))?;
    if args.get("expected_revision").is_some() {
        spec = spec
            .with_expected_revision(arg_str(args, "expected_revision")?.to_owned())
            .map_err(|_| {
                ToolError::new("expected_revision must be the revision token returned by read")
            })?;
    }
    let output = filesystem.read(spec, cx.cancellation.clone()).await.map_err(|error| {
        match error.code() {
            heycode_exec::FileSystemErrorCode::Binary => ToolError::new(format!("{raw} may be binary; it cannot be shown as text")),
            heycode_exec::FileSystemErrorCode::StaleObservation => ToolError::new(format!("{raw} changed since the previous read. Read the relevant section again before continuing or editing.")),
            heycode_exec::FileSystemErrorCode::InvalidSpec => ToolError::new("Requested page is outside the bounded 64 MiB scan, or the byte cap cannot fit one UTF-8 character. Use grep to narrow the file or increase max_bytes."),
            _ => filesystem_error(&error, "read", raw, Some(path.as_path())),
        }
    })?;
    let page = output.page().ok_or_else(|| {
        ToolError::new("Filesystem provider does not support paginated text reads")
    })?;
    let text = std::str::from_utf8(output.bytes())
        .map_err(|_| ToolError::new("Read output is not valid UTF-8"))?;
    let content = text
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{:>4}\t{line}", page.start_line + index))
        .collect::<Vec<_>>()
        .join("\n");
    let continuation = if let Some(offset) = page.next_offset {
        json!({"path":raw,"offset":offset,"limit":limit,"max_bytes":max_bytes,"expected_revision":page.revision})
    } else if let Some(byte_offset) = page.next_byte_offset {
        json!({"path":raw,"byte_offset":byte_offset,"limit":limit,"max_bytes":max_bytes,"expected_revision":page.revision})
    } else {
        Value::Null
    };
    let completed_through = page
        .start_line
        .saturating_sub(1)
        .saturating_add(page.lines_returned)
        .saturating_sub(usize::from(page.partial_last_line));
    let lines_remaining = page
        .total_lines
        .map(|total| total.saturating_sub(completed_through));
    let newlines = output.bytes().iter().filter(|byte| **byte == b'\n').count();
    let crlf = output
        .bytes()
        .windows(2)
        .filter(|pair| *pair == b"\r\n")
        .count();
    let page_line_ending = if newlines == 0 {
        "none"
    } else if crlf == newlines {
        "crlf"
    } else if crlf == 0 {
        "lf"
    } else {
        "mixed"
    };
    Ok(json!({
        "path":raw,"content":content,"offset":page.start_line,"lines_returned":page.lines_returned,"lines_remaining":lines_remaining,"line_limit":limit,
        "total_lines":page.total_lines,"total_bytes":page.total_bytes,"bytes_returned":output.bytes().len(),"revision":page.revision,"page_line_ending":page_line_ending,
        "truncated":output.truncated(),"next_offset":page.next_offset,"next_byte_offset":page.next_byte_offset,
        "partial_last_line":page.partial_last_line,"scan_limited":page.scan_limited,"continuation":continuation
    }))
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus { configured: Some(true), detail: "Filesystem service is bound; paths, sandbox, permissions and operation preconditions are checked per invocation.".into() }
    }

    fn effect(&self) -> crate::ToolEffect {
        crate::ToolEffect::ReadOnly
    }

    fn rebind_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(tool(self.cfg.clone(), filesystem.clone()))
    }

    fn spec(&self) -> ToolSpec {
        spec()
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        read_page(&self.cfg, &self.filesystem, &args, cx).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    async fn read_with_cfg(cfg: ToolsConfig, dir: &Path, args: Value) -> Result<Value, ToolError> {
        let tool = tool(Arc::new(cfg), crate::builtins::test_filesystem(dir));
        let cx = ToolCtx {
            cwd: dir.to_path_buf(),
            ..Default::default()
        };
        tool.run(args, &cx).await
    }

    #[tokio::test]
    async fn numbers_lines_in_cat_n_style() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let out = read_with_cfg(ToolsConfig::default(), dir.path(), json!({"path": "f.txt"}))
            .await
            .unwrap();
        assert_eq!(
            out["content"],
            json!("   1\talpha\n   2\tbeta\n   3\tgamma")
        );
        assert_eq!(out["total_lines"], 3);
    }

    #[tokio::test]
    async fn missing_file_names_the_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path": "nope.txt"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.message.contains("File not found:"),
            "got: {}",
            err.message
        );
        assert!(
            err.message
                .contains(&dir.path().join("nope.txt").display().to_string())
        );
    }

    #[tokio::test]
    async fn caps_lines_and_appends_footer() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=5).map(|i| format!("line{i}\n")).collect();
        std::fs::write(dir.path().join("f.txt"), body).unwrap();
        let cfg = ToolsConfig {
            read_max_lines: 2,
            ..ToolsConfig::default()
        };
        let out = read_with_cfg(cfg, dir.path(), json!({"path": "f.txt"}))
            .await
            .unwrap();
        let text = out["content"].as_str().unwrap();
        assert!(text.contains("   1\tline1"));
        assert!(text.contains("   2\tline2"));
        assert!(!text.contains("line3"), "capped lines must not appear");
        assert_eq!(out["total_lines"], 5);
        assert_eq!(out["next_offset"], 3);
    }

    #[tokio::test]
    async fn caps_bytes_and_appends_truncation_footer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "abcdefghij").unwrap();
        let cfg = ToolsConfig {
            read_max_bytes: 4,
            ..ToolsConfig::default()
        };
        let out = read_with_cfg(cfg, dir.path(), json!({"path": "f.txt"}))
            .await
            .unwrap();
        let text = out["content"].as_str().unwrap();
        assert!(text.starts_with("   1\tabcd"));
        assert_eq!(out["truncated"], true);
        assert_eq!(out["next_byte_offset"], 4);
    }

    #[tokio::test]
    async fn flags_binary_files_instead_of_dumping_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut blob = vec![0u8; 8192];
        blob.extend_from_slice(b"tail");
        std::fs::write(dir.path().join("blob.bin"), &blob).unwrap();
        let err = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path": "blob.bin"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.message.contains("may be binary"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn marks_read_paths_as_observed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("watched.txt");
        std::fs::write(&file, "hi\n").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        let log = filesystem.observations();
        let tool = tool(Arc::new(ToolsConfig::default()), filesystem);
        let cx = ToolCtx {
            cwd: dir.path().to_path_buf(),
            ..Default::default()
        };
        tool.run(json!({"path": "watched.txt"}), &cx).await.unwrap();
        assert!(log.contains(&file), "read must record the observation");
    }

    #[test]
    fn spec_declares_required_object_schema() {
        let s = spec();
        assert_eq!(s.name, "read");
        assert!(
            s.parameters["required"]
                .as_array()
                .unwrap()
                .contains(&json!("path"))
        );
    }

    #[tokio::test]
    async fn default_read_is_two_hundred_lines_and_continuation_reaches_later_pages() {
        let dir = tempfile::tempdir().unwrap();
        let body = (1..=437)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        std::fs::write(dir.path().join("large.txt"), &body).unwrap();
        let first = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path":"large.txt"}),
        )
        .await
        .unwrap();
        assert_eq!(first["lines_returned"], 200);
        assert_eq!(first["total_lines"], 437);
        assert_eq!(first["lines_remaining"], 237);
        assert_eq!(first["total_bytes"], body.len());
        assert_eq!(first["next_offset"], 201);
        assert!(!first["content"].as_str().unwrap().contains("line 201"));
        let second = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            first["continuation"].clone(),
        )
        .await
        .unwrap();
        assert_eq!(second["offset"], 201);
        assert_eq!(second["next_offset"], 401);
        let last = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            second["continuation"].clone(),
        )
        .await
        .unwrap();
        assert_eq!(last["lines_returned"], 37);
        assert_eq!(last["lines_remaining"], 0);
        assert_eq!(last["continuation"], Value::Null);
        assert!(last["content"].as_str().unwrap().ends_with("line 437"));
    }

    #[tokio::test]
    async fn long_utf8_line_can_be_reassembled_without_loss_or_replacement_characters() {
        let dir = tempfile::tempdir().unwrap();
        let body = "é🙂z".repeat(7000);
        std::fs::write(dir.path().join("long.txt"), &body).unwrap();
        let mut args = json!({"path":"long.txt"});
        let mut joined = String::new();
        for _ in 0..10 {
            let page = read_with_cfg(ToolsConfig::default(), dir.path(), args)
                .await
                .unwrap();
            let text = page["content"]
                .as_str()
                .unwrap()
                .split_once('\t')
                .unwrap()
                .1;
            assert!(!text.contains('\u{fffd}'));
            assert!(text.len() <= DEFAULT_READ_BYTES);
            joined.push_str(text);
            args = page["continuation"].clone();
            if args.is_null() {
                break;
            }
        }
        assert_eq!(joined, body);
    }

    #[tokio::test]
    async fn changed_page_revision_refuses_continuation_and_empty_or_past_eof_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
        let page = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path":"f.txt","limit":1}),
        )
        .await
        .unwrap();
        std::fs::write(dir.path().join("f.txt"), "changed\nb\nc\n").unwrap();
        let error = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            page["continuation"].clone(),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("changed"));
        let past = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path":"f.txt","offset":100}),
        )
        .await
        .unwrap();
        assert_eq!(past["lines_returned"], 0);
        assert_eq!(past["total_lines"], 3);
        assert!(past["continuation"].is_null());
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let empty = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path":"empty.txt"}),
        )
        .await
        .unwrap();
        assert_eq!(empty["total_lines"], 0);
        assert_eq!(empty["total_bytes"], 0);
    }

    #[tokio::test]
    async fn invalid_pagination_arguments_fail_instead_of_silently_dumping_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "sample").unwrap();
        for args in [
            json!({"path":"f.txt","offset":0}),
            json!({"path":"f.txt","limit":-1}),
            json!({"path":"f.txt","limit":2001}),
            json!({"path":"f.txt","offset":1,"byte_offset":0}),
            json!({"path":"f.txt","max_bytes":"large"}),
        ] {
            assert!(
                read_with_cfg(ToolsConfig::default(), dir.path(), args)
                    .await
                    .is_err()
            );
        }
    }
    #[tokio::test]
    async fn crlf_pages_keep_exact_byte_counts_and_custom_continuation_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "é\r\nβ\r\nlast").unwrap();
        let page = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            json!({"path":"f.txt","limit":1,"max_bytes":8}),
        )
        .await
        .unwrap();
        assert_eq!(page["total_lines"], 3);
        assert_eq!(page["lines_remaining"], 2);
        assert_eq!(page["total_bytes"], 12);
        assert_eq!(page["bytes_returned"], 4);
        assert_eq!(page["page_line_ending"], "crlf");
        assert_eq!(page["continuation"]["max_bytes"], 8);
        let next = read_with_cfg(
            ToolsConfig::default(),
            dir.path(),
            page["continuation"].clone(),
        )
        .await
        .unwrap();
        assert_eq!(next["offset"], 2);
        assert_eq!(next["lines_remaining"], 1);
        assert!(next["content"].as_str().unwrap().ends_with("β"));
    }
}
