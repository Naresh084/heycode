//! `edit` — exact-string replacement in an already-read file.

use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Value, json};

use crate::builtins::{arg_str, filesystem_error, resolve_path};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) fn tool(filesystem: heycode_exec::FileSystemService) -> Arc<dyn Tool> {
    Arc::new(EditTool { filesystem })
}

struct EditTool {
    filesystem: heycode_exec::FileSystemService,
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "edit".to_owned(),
        description: "Replace an exact string in a file you have read this session. \
                      old_string must match exactly once unless replace_all is true."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                "old_string": {"type": "string", "description": "Exact text to replace, including whitespace."},
                "new_string": {"type": "string", "description": "Replacement text; empty string deletes the old text."},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one."},
                "expected_revision": {"type":"string","pattern":"^[0-9a-f]{64}$","description":"Revision returned by read; refuses stale edits."},
                "dry_run": {"type":"boolean","description":"Preview without changing the file."}
            },
            "required": ["path", "old_string", "new_string"]
        }),
    }
}

#[async_trait::async_trait]
impl Tool for EditTool {
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
        let old = arg_str(&args, "old_string")?;
        let new = arg_str(&args, "new_string")?;
        let replace_all = super::multi_edit::bool_arg(&args, "replace_all")?;
        if old.is_empty() {
            return Err(ToolError::new("old_string must not be empty"));
        }
        let path = resolve_path(&self.filesystem, cx, raw)?;
        let spec = heycode_exec::EditFileSpec::new(path.clone(), old, new, replace_all)
            .map_err(|error| filesystem_error(&error, "edit", raw, Some(path.as_path())))?;
        let mut batch = heycode_exec::MultiEditSpec::new(vec![spec])
            .map_err(|_| ToolError::new("Edit payload exceeds 256 KiB"))?
            .with_dry_run(super::multi_edit::bool_arg(&args, "dry_run")?);
        if args.get("expected_revision").is_some() {
            batch = batch
                .with_expected_revision(arg_str(&args, "expected_revision")?.to_owned())
                .map_err(|_| ToolError::new("expected_revision must be returned by read"))?;
        }
        let batch_output = self
            .filesystem
            .edit_many(batch, cx.cancellation.clone())
            .await
            .map_err(|error| match error.code() {
                heycode_exec::FileSystemErrorCode::NotObserved => {
                    ToolError::new(format!("Read {raw} before editing."))
                }
                heycode_exec::FileSystemErrorCode::StaleObservation => ToolError::new(format!(
                    "{raw} changed on disk since you read it — re-read it before editing."
                )),
                heycode_exec::FileSystemErrorCode::InvalidUtf8 => ToolError::new(format!(
                    "{} is not valid UTF-8; it cannot be edited as text",
                    path.as_path().display()
                )),
                heycode_exec::FileSystemErrorCode::MatchCount => {
                    let count = error.match_count().unwrap_or(0);
                    ToolError::new(format!(
                        "old_string matched {count} time(s) in {raw}; expected exactly 1 — extend old_string until unique, or pass replace_all=true"
                    ))
                }
                _ => filesystem_error(&error, "edit", raw, Some(path.as_path())),
            })?;
        let output = batch_output
            .edits
            .first()
            .ok_or_else(|| ToolError::new("Missing edit result"))?;
        let diff = format!(
            "{}\n{}",
            prefixed_diff('-', output.removed_line()),
            prefixed_diff('+', output.inserted_line())
        );
        Ok(serde_json::json!({
            "message": format!("{} {raw} ({} replacement(s))", if batch_output.dry_run {"Previewed"} else if !batch_output.changed {"Unchanged"} else {"Edited"}, output.replacements()),
            "diff": diff,
            "diff_preview": edit_preview(output, &mut 16384),
            "line": output.line(),"diff_truncated":output.diff_truncated(),"revision":batch_output.revision,"dry_run":batch_output.dry_run,"changed":batch_output.changed
        }))
    }
}

/// Structured display rows are separate from the exact-string mutation input.
/// Keep one shared text budget across all edits in a batch.
pub(crate) fn edit_preview(
    output: &heycode_exec::EditFileOutput,
    budget: &mut usize,
) -> serde_json::Value {
    use serde_json::json;
    let Some(context) = output.context() else {
        return serde_json::Value::Null;
    };
    let mut rows = Vec::new();
    let mut truncated = output.diff_truncated();
    let mut append = |kind: &str, first: usize, text: Vec<&str>| {
        for (offset, text) in text.into_iter().enumerate() {
            if rows.len() >= 24 || *budget == 0 {
                truncated = true;
                break;
            }
            let mut end = text
                .len()
                .min(*budget)
                .min(if kind == "context" { 512 } else { 4096 });
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            truncated |= end < text.len();
            rows.push(json!({"kind":kind,"line":first.saturating_add(offset),"text":&text[..end]}));
            *budget -= end;
        }
    };
    append(
        "context",
        output.line().saturating_sub(context.before.len()),
        context.before.iter().map(String::as_str).collect(),
    );
    if context.removed_lines > 0 {
        append(
            "removed",
            output.line(),
            output.removed_line().split('\n').collect(),
        );
    }
    if context.inserted_lines > 0 {
        append(
            "added",
            output.line(),
            output.inserted_line().split('\n').collect(),
        );
    }
    append(
        "context",
        output.line().saturating_add(context.inserted_lines),
        context.after.iter().map(String::as_str).collect(),
    );
    json!({"rows":rows,"removed_lines":context.removed_lines,"inserted_lines":context.inserted_lines,
        "first_replacement_only":output.replacements()>1,"truncated":truncated})
}

pub(crate) fn prefixed_diff(marker: char, text: &str) -> String {
    text.split('\n')
        .map(|line| format!("{marker}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn run_edit(
        dir: &Path,
        filesystem: &heycode_exec::FileSystemService,
        args: Value,
    ) -> Result<Value, ToolError> {
        tool(filesystem.clone())
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
    async fn refuses_to_edit_a_file_that_was_never_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        let err = run_edit(
            dir.path(),
            &filesystem,
            json!({"path": "f.txt", "old_string": "hello", "new_string": "goodbye"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.message, "Read f.txt before editing.");
    }

    #[tokio::test]
    async fn replaces_the_single_occurrence_after_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "hello world\n").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let out = run_edit(
            dir.path(),
            &filesystem,
            json!({"path": "f.txt", "old_string": "world", "new_string": "rust"}),
        )
        .await
        .unwrap();
        assert_eq!(out["message"], json!("Edited f.txt (1 replacement(s))"));
        let diff = out["diff"].as_str().unwrap();
        assert!(diff.contains("-hello world"));
        assert!(diff.contains("+hello rust"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello rust\n");
    }

    #[tokio::test]
    async fn ambiguous_old_string_fails_naming_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "aa bb aa").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let err = run_edit(
            dir.path(),
            &filesystem,
            json!({"path": "f.txt", "old_string": "aa", "new_string": "z"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.message.contains("matched 2 time(s)"),
            "got: {}",
            err.message
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "aa bb aa",
            "failed edit must not write"
        );
    }

    #[tokio::test]
    async fn missing_old_string_also_names_zero_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "only here").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let err = run_edit(
            dir.path(),
            &filesystem,
            json!({"path": "f.txt", "old_string": "absent", "new_string": "x"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.message.contains("matched 0 time(s)"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn replace_all_swaps_every_occurrence_and_reports_count() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "aa bb aa cc aa").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let out = run_edit(
            dir.path(),
            &filesystem,
            json!({"path": "f.txt", "old_string": "aa", "new_string": "zz", "replace_all": true}),
        )
        .await
        .unwrap();
        assert_eq!(out["message"], json!("Edited f.txt (3 replacement(s))"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "zz bb zz cc zz");
    }

    #[tokio::test]
    async fn multiline_replacement_commits_and_prefixes_every_diff_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "start\nold one\nold two\nend\n").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let out = run_edit(
            dir.path(),
            &filesystem,
            json!({
                "path": "f.txt",
                "old_string": "old one\nold two",
                "new_string": "new one\nnew two\nnew three"
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            out["diff"],
            json!("-old one\n-old two\n+new one\n+new two\n+new three")
        );
        assert_eq!(out["line"], 2);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "start\nnew one\nnew two\nnew three\nend\n"
        );
    }

    #[tokio::test]
    async fn replace_all_still_requires_at_least_one_match() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "unchanged").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let error = run_edit(
            dir.path(),
            &filesystem,
            json!({
                "path": "f.txt",
                "old_string": "missing",
                "new_string": "new",
                "replace_all": true
            }),
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("matched 0 time(s)"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "unchanged");
    }

    #[tokio::test]
    async fn replacement_ending_at_a_line_boundary_has_an_exact_diff() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "old\nnext\n").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        filesystem.observations().mark(&file);
        let out = run_edit(
            dir.path(),
            &filesystem,
            json!({
                "path": "f.txt",
                "old_string": "old\n",
                "new_string": "new\n"
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["diff"], json!("-old\n+new"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nnext\n");
    }

    #[test]
    fn spec_requires_the_three_core_arguments() {
        let s = spec();
        let required: Vec<&str> = s.parameters["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required, vec!["path", "old_string", "new_string"]);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod staleness_tests {
    use super::*;
    use crate::tool::ToolCtx;

    /// The read-before-edit gate was PATH-only: any prior read authorized an
    /// edit forever, so a file that changed under the model was edited against
    /// content the model had never seen.
    #[tokio::test]
    async fn edit_refuses_a_file_that_changed_since_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        let p = path.display().to_string();

        let filesystem = crate::builtins::test_filesystem(dir.path());
        let observations = filesystem.observations();
        let edit = tool(filesystem);
        observations.mark(&path);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "hello mars\n").unwrap();

        let err = edit
            .run(
                serde_json::json!({"path": p, "old_string": "hello", "new_string": "goodbye"}),
                &ToolCtx::default(),
            )
            .await
            .expect_err("editing a changed file must be refused");
        assert!(
            err.message.contains("changed") || err.message.contains("re-read"),
            "error must tell the model to re-read: {}",
            err.message
        );
    }

    /// A read with no intervening change still authorizes the edit.
    #[tokio::test]
    async fn edit_allows_a_file_that_is_unchanged_since_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        let p = path.display().to_string();

        let filesystem = crate::builtins::test_filesystem(dir.path());
        let observations = filesystem.observations();
        let edit = tool(filesystem);
        observations.mark(&path);

        edit.run(
            serde_json::json!({"path": p, "old_string": "world", "new_string": "rust"}),
            &ToolCtx::default(),
        )
        .await
        .expect("unchanged file must remain editable");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust\n");
    }
}
