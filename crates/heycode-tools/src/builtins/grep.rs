//! `grep` — regex content search over the workspace tree.

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Value, json};

use crate::builtins::{GREP_CAP, SKIP_DIRS, arg_str, filesystem_error, resolve_path};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) fn tool(filesystem: heycode_exec::FileSystemService) -> Arc<dyn Tool> {
    Arc::new(GrepTool { filesystem })
}

struct GrepTool {
    filesystem: heycode_exec::FileSystemService,
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "grep".to_owned(),
        description: "Bounded regular-expression search compatible with Claude's core Grep shapes. `output_mode` defaults to files_with_matches; content prints `path:line:text`, and count prints per-file matching-line counts. `glob` filters paths; legacy `include` remains an exact alias. Content mode supports -A/-B/-C/context (at most 20 lines) and -n; -i applies to every mode. head_limit is at most 200 and offset at most 100000. Vendored directories are skipped; retained output caps at 24 KiB, each file scans at most 64 MiB, and the aggregate scan at 256 MiB. Partial coverage is reported. Unsupported Claude options such as type, multiline, and only_matching are not advertised and fail if called directly."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "maxLength": 65536, "description": "Rust `regex` pattern to search for."},
                "path": {"type": "string", "maxLength": 4096, "description": "File or directory to search; defaults to the working directory."},
                "glob": {"type": "string", "maxLength": 65536, "description": "Path or filename glob filter such as `*.rs` or `src/**/*.rs`."},
                "include": {"type": "string", "maxLength": 65536, "description": "Legacy exact alias for glob; do not pass both with different values."},
                "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"], "default": "files_with_matches", "description": "content shows matching lines, files_with_matches shows paths, count shows per-file matching-line counts."},
                "-A": {"type": "integer", "minimum": 0, "maximum": 20, "description": "Content mode only: following source lines per match."},
                "-B": {"type": "integer", "minimum": 0, "maximum": 20, "description": "Content mode only: preceding source lines per match."},
                "-C": {"type": "integer", "minimum": 0, "maximum": 20, "description": "Alias for context."},
                "context": {"type": "integer", "minimum": 0, "maximum": 20, "description": "Content mode only: default preceding and following source lines per match."},
                "-n": {"type": "boolean", "default": true, "description": "Content mode only: include one-based line numbers."},
                "-i": {"type": "boolean", "default": false, "description": "Case-insensitive regular-expression matching."},
                "head_limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 200, "description": "Maximum retained matching lines or file entries."},
                "offset": {"type": "integer", "minimum": 0, "maximum": 100000, "default": 0, "description": "Matching lines or file entries to skip before head_limit is applied."}
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    }
}

fn optional_str<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    args.get(name)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ToolError::new(format!("{name} must be a string")))
        })
        .transpose()
}

fn optional_bool(args: &Value, name: &str, default: bool) -> Result<bool, ToolError> {
    args.get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolError::new(format!("{name} must be a boolean")))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn optional_nonnegative(
    args: &Value,
    name: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, ToolError> {
    args.get(name)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value <= maximum)
                .ok_or_else(|| {
                    ToolError::new(format!("{name} must be an integer from 0 to {maximum}"))
                })
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn output_mode(args: &Value) -> Result<heycode_exec::GrepOutputMode, ToolError> {
    match optional_str(args, "output_mode")?.unwrap_or("files_with_matches") {
        "content" => Ok(heycode_exec::GrepOutputMode::Content),
        "files_with_matches" => Ok(heycode_exec::GrepOutputMode::FilesWithMatches),
        "count" => Ok(heycode_exec::GrepOutputMode::Count),
        _ => Err(ToolError::new(
            "output_mode must be content, files_with_matches, or count",
        )),
    }
}

fn append_line(output: &mut String, line: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(line);
}

fn format_content(
    result: &heycode_exec::GrepOutput,
    line_numbers: bool,
    context_requested: bool,
) -> String {
    let mut rows = BTreeMap::<(&str, usize), (bool, &str)>::new();
    for entry in result.matches() {
        for line in entry.before() {
            rows.entry((entry.path(), line.line()))
                .or_insert((false, line.text()));
        }
        rows.insert((entry.path(), entry.line()), (true, entry.text()));
        for line in entry.after() {
            rows.entry((entry.path(), line.line()))
                .or_insert((false, line.text()));
        }
    }
    let mut output = String::new();
    let mut previous = None::<(&str, usize)>;
    for ((path, line), (matched, text)) in rows {
        if context_requested
            && previous.is_some_and(|(previous_path, previous_line)| {
                previous_path != path || previous_line.saturating_add(1) < line
            })
        {
            append_line(&mut output, "--");
        }
        let delimiter = if matched { ':' } else { '-' };
        let row = if line_numbers {
            format!("{path}{delimiter}{line}{delimiter}{text}")
        } else {
            format!("{path}{delimiter}{text}")
        };
        append_line(&mut output, &row);
        previous = Some((path, line));
    }
    output
}

fn append_pagination(
    output: &mut String,
    retained: usize,
    total: usize,
    offset: usize,
    limit: usize,
    incomplete: bool,
    noun: &str,
) {
    if offset == 0 && retained == total {
        return;
    }
    let selected = total.saturating_sub(offset).min(limit);
    if selected == 0 {
        append_line(
            output,
            &format!("No entries at offset {offset}; observed {total} {noun}."),
        );
        return;
    }
    let first = offset.saturating_add(1);
    let last = offset.saturating_add(selected);
    let total_label = if incomplete {
        format!("at least {total}")
    } else {
        total.to_string()
    };
    if retained < selected {
        append_line(
            output,
            &format!(
                "(retained {retained} of selected entries {first}-{last} from {total_label} {noun}; offset {offset}, limit {limit}; output safety cap applied)"
            ),
        );
        return;
    }
    append_line(
        output,
        &format!(
            "(showing {first}-{last} of {total_label} {noun}; offset {offset}, limit {limit})",
        ),
    );
}

#[async_trait::async_trait]
impl Tool for GrepTool {
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
        Some(tool(filesystem.clone()))
    }

    fn spec(&self) -> ToolSpec {
        spec()
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let Some(arguments) = args.as_object() else {
            return Err(ToolError::new("grep arguments must be an object"));
        };
        const SUPPORTED: &[&str] = &[
            "pattern",
            "path",
            "glob",
            "include",
            "output_mode",
            "-A",
            "-B",
            "-C",
            "context",
            "-n",
            "-i",
            "head_limit",
            "offset",
        ];
        if let Some(name) = arguments
            .keys()
            .find(|name| !SUPPORTED.contains(&name.as_str()))
        {
            return Err(ToolError::new(format!(
                "unsupported grep argument {name:?}; type, multiline, and only_matching are not implemented"
            )));
        }
        let pattern = arg_str(&args, "pattern")?;
        let glob = optional_str(&args, "glob")?;
        let include = optional_str(&args, "include")?;
        if glob.is_some() && include.is_some() && glob != include {
            return Err(ToolError::new(
                "glob and include are aliases and must not disagree",
            ));
        }
        let include = glob.or(include);
        let raw_root = optional_str(&args, "path")?.unwrap_or(".");
        if raw_root.len() > 4096 {
            return Err(ToolError::new("path exceeds 4096 bytes"));
        }
        let mode = output_mode(&args)?;
        let combined_context = match (args.get("-C").is_some(), args.get("context").is_some()) {
            (true, true) => {
                let short = optional_nonnegative(&args, "-C", 0, 20)?;
                let named = optional_nonnegative(&args, "context", 0, 20)?;
                if short != named {
                    return Err(ToolError::new("-C and context must not disagree"));
                }
                short
            }
            (true, false) => optional_nonnegative(&args, "-C", 0, 20)?,
            (false, true) => optional_nonnegative(&args, "context", 0, 20)?,
            (false, false) => 0,
        };
        let before_context = optional_nonnegative(&args, "-B", combined_context, 20)?;
        let after_context = optional_nonnegative(&args, "-A", combined_context, 20)?;
        let content_only_requested = ["-A", "-B", "-C", "context", "-n"]
            .iter()
            .any(|name| args.get(name).is_some());
        if mode != heycode_exec::GrepOutputMode::Content && content_only_requested {
            return Err(ToolError::new(
                "-A, -B, -C/context, and -n require output_mode=content",
            ));
        }
        let line_numbers = optional_bool(&args, "-n", true)?;
        let case_insensitive = optional_bool(&args, "-i", false)?;
        let head_limit = optional_nonnegative(&args, "head_limit", GREP_CAP, GREP_CAP)?;
        if head_limit == 0 {
            return Err(ToolError::new(
                "head_limit must be an integer from 1 to 200",
            ));
        }
        let offset = optional_nonnegative(&args, "offset", 0, 100_000)?;
        let root = resolve_path(&self.filesystem, cx, raw_root)?;
        let spec =
            heycode_exec::GrepSpec::new(root.clone(), pattern, include, SKIP_DIRS, head_limit)
                .and_then(|spec| {
                    spec.with_options(
                        mode,
                        offset,
                        before_context,
                        after_context,
                        case_insensitive,
                    )
                })
                .map_err(|error| {
                    filesystem_error(&error, "search", raw_root, Some(root.as_path()))
                })?;
        let result = self
            .filesystem
            .grep(spec, cx.cancellation.clone())
            .await
            .map_err(|error| {
                if error.code() == heycode_exec::FileSystemErrorCode::Pattern {
                    ToolError::new(format!(
                        "invalid regular expression {pattern:?} — escape metacharacters like . ( ) [ ] * with backslashes, or simplify the pattern"
                    ))
                } else {
                    filesystem_error(&error, "search", raw_root, Some(root.as_path()))
                }
            })?;
        let (mut out, retained, total, noun) = match mode {
            heycode_exec::GrepOutputMode::Content => (
                format_content(
                    &result,
                    line_numbers,
                    before_context > 0 || after_context > 0,
                ),
                result.matches().len(),
                result.total_matches(),
                "matching lines",
            ),
            heycode_exec::GrepOutputMode::FilesWithMatches => (
                result
                    .files()
                    .iter()
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>()
                    .join("\n"),
                result.files().len(),
                result.total_files(),
                "matching files",
            ),
            heycode_exec::GrepOutputMode::Count => (
                result
                    .files()
                    .iter()
                    .map(|entry| format!("{}:{}", entry.path(), entry.count()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                result.files().len(),
                result.total_files(),
                "matching files",
            ),
        };
        append_pagination(
            &mut out,
            retained,
            total,
            offset,
            head_limit,
            result.report().incomplete(),
            noun,
        );
        if let Some(notice) = result.report().notice() {
            append_line(&mut out, &notice);
        }
        Ok(Value::String(out))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn run_grep(dir: &Path, args: Value) -> Result<Value, ToolError> {
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

    async fn seeded_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("src/main.rs", "fn main() {\n    // needle here\n}\n"),
            (
                "docs/guide.md",
                "the needle hides in prose\nno match line\n",
            ),
            ("node_modules/lib.js", "needle in vendored code\n"),
            ("target/out.log", "needle in build output\n"),
        ];
        for (f, body) in files {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn content_mode_prints_path_line_and_source_text() {
        let dir = seeded_tree().await;
        let out = run_grep(
            dir.path(),
            json!({"pattern": "needle", "output_mode": "content"}),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            json!("docs/guide.md:1:the needle hides in prose\nsrc/main.rs:2:    // needle here")
        );
    }

    #[tokio::test]
    async fn default_mode_lists_matching_files() {
        let dir = seeded_tree().await;
        let out = run_grep(dir.path(), json!({"pattern": "needle"}))
            .await
            .unwrap();
        assert_eq!(out, json!("docs/guide.md\nsrc/main.rs"));
    }

    #[tokio::test]
    async fn skips_vendored_directories() {
        let dir = seeded_tree().await;
        let out = run_grep(dir.path(), json!({"pattern": "needle", "include": "*.js"}))
            .await
            .unwrap();
        assert_eq!(out, json!(""), "node_modules must never be searched");
    }

    #[tokio::test]
    async fn include_filter_restricts_searched_files() {
        let dir = seeded_tree().await;
        let out = run_grep(dir.path(), json!({"pattern": "needle", "include": "*.rs"}))
            .await
            .unwrap();
        assert_eq!(out, json!("src/main.rs"));
    }

    #[tokio::test]
    async fn single_file_target_uses_the_file_name_as_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "hit\n").unwrap();
        let out = run_grep(dir.path(), json!({"pattern": "hit", "path": "one.txt"}))
            .await
            .unwrap();
        assert_eq!(out, json!("one.txt"));
    }

    #[tokio::test]
    async fn caps_matches_with_a_footer() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..250).map(|i| format!("needle{i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), body).unwrap();
        let out = run_grep(
            dir.path(),
            json!({"pattern": "needle", "output_mode": "content"}),
        )
        .await
        .unwrap();
        let text = out.as_str().unwrap();
        assert_eq!(text.lines().count(), 201, "200 matches plus the footer");
        assert!(
            text.contains("(showing 1-200 of 250 matching lines; offset 0, limit 200)"),
            "got tail: {}",
            &text[text.len().saturating_sub(60)..]
        );
    }

    #[tokio::test]
    async fn invalid_regex_fails_with_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_grep(dir.path(), json!({"pattern": "(unclosed"}))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("invalid regular expression"),
            "got: {}",
            err.message
        );
        assert!(err.message.contains("escape"), "must hint at escaping");
    }

    #[tokio::test]
    async fn binary_files_are_skipped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let mut blob = vec![0u8; 100];
        blob.extend_from_slice(b"needle");
        std::fs::write(dir.path().join("blob.bin"), &blob).unwrap();
        let out = run_grep(dir.path(), json!({"pattern": "needle"}))
            .await
            .unwrap();
        assert_eq!(out, json!(""));
    }

    #[tokio::test]
    async fn partial_search_notice_reaches_model_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'x'; 1024 * 1024 + 1];
        bytes.extend_from_slice(b"\nneedle\n");
        std::fs::write(dir.path().join("oversized.txt"), bytes).unwrap();
        let out = run_grep(
            dir.path(),
            json!({"pattern": "needle", "output_mode": "content"}),
        )
        .await
        .unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("oversized.txt:2:needle"));
        assert!(text.contains("Partial search; match count is a lower bound"));
        assert!(text.contains("1 oversized lines skipped"));
    }

    #[tokio::test]
    async fn count_and_case_insensitive_modes_compose() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "Needle\nnone\nneedle\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "NEEDLE\n").unwrap();
        let out = run_grep(
            dir.path(),
            json!({"pattern": "needle", "output_mode": "count", "-i": true}),
        )
        .await
        .unwrap();
        assert_eq!(out, json!("a.txt:2\nb.txt:1"));
    }

    #[tokio::test]
    async fn content_context_merges_rows_and_marks_group_gaps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sample.txt"),
            "first\nNeedle one\nmiddle\nneedle two\nlast\ngap-a\ngap-b\nneedle three\nend\n",
        )
        .unwrap();
        let out = run_grep(
            dir.path(),
            json!({
                "pattern": "needle",
                "output_mode": "content",
                "context": 1,
                "-i": true
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            json!(concat!(
                "sample.txt-1-first\n",
                "sample.txt:2:Needle one\n",
                "sample.txt-3-middle\n",
                "sample.txt:4:needle two\n",
                "sample.txt-5-last\n",
                "--\n",
                "sample.txt-7-gap-b\n",
                "sample.txt:8:needle three\n",
                "sample.txt-9-end"
            ))
        );
    }

    #[tokio::test]
    async fn content_can_omit_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sample.txt"), "before\nneedle\nafter\n").unwrap();
        let out = run_grep(
            dir.path(),
            json!({
                "pattern": "needle",
                "output_mode": "content",
                "-C": 1,
                "-n": false
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            json!("sample.txt-before\nsample.txt:needle\nsample.txt-after")
        );
    }

    #[tokio::test]
    async fn path_glob_and_pagination_are_bounded_and_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in [
            ("src/main.rs", "needle1\nneedle2\nneedle3\nneedle4\n"),
            ("src/deep/lib.rs", "needle\n"),
            ("tests/main.rs", "needle\n"),
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let files = run_grep(
            dir.path(),
            json!({"pattern": "needle", "glob": "src/**/*.rs"}),
        )
        .await
        .unwrap();
        assert_eq!(files, json!("src/deep/lib.rs\nsrc/main.rs"));

        let content = run_grep(
            dir.path(),
            json!({
                "pattern": "needle",
                "path": "src/main.rs",
                "output_mode": "content",
                "head_limit": 2,
                "offset": 1
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            content,
            json!(concat!(
                "main.rs:2:needle2\n",
                "main.rs:3:needle3\n",
                "(showing 2-3 of 4 matching lines; offset 1, limit 2)"
            ))
        );
    }

    #[tokio::test]
    async fn conflicting_or_unsupported_options_fail_explicitly() {
        let dir = seeded_tree().await;
        for (args, message) in [
            (
                json!({"pattern": "needle", "glob": "*.rs", "include": "*.md"}),
                "aliases",
            ),
            (
                json!({
                    "pattern": "needle",
                    "output_mode": "content",
                    "-C": 1,
                    "context": 2
                }),
                "must not disagree",
            ),
            (
                json!({"pattern": "needle", "-n": false}),
                "require output_mode=content",
            ),
            (
                json!({"pattern": "needle", "only_matching": true}),
                "unsupported grep argument",
            ),
        ] {
            let error = run_grep(dir.path(), args).await.unwrap_err();
            assert!(
                error.message.contains(message),
                "expected {message:?}, got {:?}",
                error.message
            );
        }
    }

    #[test]
    fn spec_requires_pattern_only() {
        let s = spec();
        assert_eq!(s.parameters["required"], json!(["pattern"]));
        assert_eq!(
            s.parameters["properties"]["output_mode"]["default"],
            json!("files_with_matches")
        );
        assert_eq!(s.parameters["additionalProperties"], json!(false));
        assert!(s.parameters["properties"].get("multiline").is_none());
    }
}
