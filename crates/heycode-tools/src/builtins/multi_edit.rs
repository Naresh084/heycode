//! Bounded previews and atomic ordered edits of one already-read file.
use super::{arg_str, filesystem_error, resolve_path};
use crate::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn tool(filesystem: heycode_exec::FileSystemService) -> Arc<dyn Tool> {
    Arc::new(MultiEdit { filesystem })
}
struct MultiEdit {
    filesystem: heycode_exec::FileSystemService,
}

pub(crate) fn bool_arg(args: &Value, name: &str) -> Result<bool, ToolError> {
    args.get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolError::new(format!("{name} must be a boolean")))
        })
        .transpose()
        .map(|value| value.unwrap_or(false))
}

pub(crate) fn edit_error(
    error: &heycode_exec::FileSystemError,
    raw: &str,
    path: &std::path::Path,
) -> ToolError {
    let message = match error.code() {
        heycode_exec::FileSystemErrorCode::NotObserved => format!("Read {raw} before editing."),
        heycode_exec::FileSystemErrorCode::StaleObservation
        | heycode_exec::FileSystemErrorCode::ChangedAtCommit => format!(
            "{raw} changed since it was read; re-read the relevant section and retry with its revision. No edit was committed."
        ),
        heycode_exec::FileSystemErrorCode::MatchCount => format!(
            "old_string matched {} time(s) in {raw}; expected exactly 1 unless replace_all=true. Use an exact, unique string. No edit was committed.",
            error.match_count().unwrap_or_default()
        ),
        _ => filesystem_error(error, "edit", raw, Some(path)).message,
    };
    ToolError::new(error.edit_index().map_or(message.clone(), |index| {
        format!("Edit {index} failed: {message}")
    }))
}

#[async_trait::async_trait]
impl Tool for MultiEdit {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name:"multi_edit".into(),
            description:"Apply 1–32 ordered exact-string edits to one file you have read. Later edits see earlier replacements. Every edit is validated before one atomic per-file commit: a failed, missing, ambiguous or stale edit leaves the file unchanged. Prefer expected_revision from Read; dry_run=true previews without writing. No fuzzy matching. Outputs bounded diffs and per-edit counts; this is not a cross-file transaction.".into(),
            parameters:json!({"type":"object","properties":{
                "path":{"type":"string","description":"One file, absolute or relative to the working directory."},
                "edits":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","properties":{
                    "old_string":{"type":"string","minLength":1,"description":"Exact unique text including whitespace."},
                    "new_string":{"type":"string","description":"Replacement text; empty deletes."},
                    "replace_all":{"type":"boolean","description":"Explicitly replace every match; at least one must exist."}
                },"required":["old_string","new_string"],"additionalProperties":false}},
                "expected_revision":{"type":"string","pattern":"^[0-9a-f]{64}$","description":"Read revision to protect against stale edits."},
                "dry_run":{"type":"boolean","description":"Preview the full edit set without changing the file."}
            },"required":["path","edits"],"additionalProperties":false}),
        }
    }
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus { configured:Some(true), detail:"Filesystem service is bound; atomic edit support, observations, revision and policy are checked per invocation.".into() }
    }
    fn rebind_workspace(
        &self,
        filesystem: &heycode_exec::FileSystemService,
        _: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(tool(filesystem.clone()))
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let raw = arg_str(&args, "path")?;
        let path = resolve_path(&self.filesystem, cx, raw)?;
        let edits = args
            .get("edits")
            .and_then(Value::as_array)
            .filter(|edits| (1..=32).contains(&edits.len()))
            .ok_or_else(|| ToolError::new("edits must contain between 1 and 32 replacements"))?;
        let edits = edits
            .iter()
            .enumerate()
            .map(|(index, edit)| {
                heycode_exec::EditFileSpec::new(
                    path.clone(),
                    arg_str(edit, "old_string")?,
                    arg_str(edit, "new_string")?,
                    bool_arg(edit, "replace_all")?,
                )
                .map_err(|_| {
                    ToolError::new(format!(
                        "Edit {} has empty old_string or an oversized payload",
                        index + 1
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut spec = heycode_exec::MultiEditSpec::new(edits)
            .map_err(|_| ToolError::new("The entire edit payload must fit within 256 KiB"))?
            .with_dry_run(bool_arg(&args, "dry_run")?);
        if args.get("expected_revision").is_some() {
            spec = spec
                .with_expected_revision(arg_str(&args, "expected_revision")?.to_owned())
                .map_err(|_| {
                    ToolError::new("expected_revision must be the token returned by read")
                })?;
        }
        let output = self
            .filesystem
            .edit_many(spec, cx.cancellation.clone())
            .await
            .map_err(|error| edit_error(&error, raw, path.as_path()))?;
        let mut diff = String::new();
        let mut diff_truncated = false;
        let mut facts = Vec::new();
        let mut previews = Vec::new();
        let mut preview_budget = 16384;
        for (index, edit) in output.edits.iter().enumerate() {
            previews.push(super::edit::edit_preview(edit, &mut preview_budget));
            facts.push(json!({"edit":index+1,"line":edit.line(),"replacements":edit.replacements(),"diff_truncated":edit.diff_truncated()}));
            let section = format!(
                "@@ edit {} · line {} @@\n{}\n{}\n",
                index + 1,
                edit.line(),
                super::edit::prefixed_diff('-', edit.removed_line()),
                super::edit::prefixed_diff('+', edit.inserted_line())
            );
            let mut end = section.len().min(16384_usize.saturating_sub(diff.len()));
            while !section.is_char_boundary(end) {
                end -= 1;
            }
            diff.push_str(&section[..end]);
            diff_truncated |= end < section.len() || edit.diff_truncated();
        }
        Ok(
            json!({"message":format!("{} {} edit(s) in {raw}{}",if output.dry_run {"Previewed"} else if !output.changed {"Validated"} else {"Applied"},output.edits.len(),if !output.changed {"; content unchanged"} else {""}),
            "edits":facts,"diff":diff,"diff_previews":previews,"diff_truncated":diff_truncated,"revision":output.revision,"dry_run":output.dry_run,"changed":output.changed}),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn model_facing_multi_edit_reports_failed_index_then_previews_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "alpha\nbeta\n").unwrap();
        let filesystem = crate::builtins::test_filesystem(dir.path());
        let cx = ToolCtx::default().with_cwd(dir.path().into());
        let page = super::super::read::read_page(
            &crate::ToolsConfig::default(),
            &filesystem,
            &json!({"path":"f.txt"}),
            &cx,
        )
        .await
        .unwrap();
        let tool = tool(filesystem);
        let error = tool.run(json!({"path":"f.txt","edits":[{"old_string":"alpha","new_string":"one"},{"old_string":"absent","new_string":"two"}]}),&cx).await.unwrap_err();
        assert!(error.message.contains("Edit 2 failed"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "alpha\nbeta\n"
        );
        let mut request = json!({"path":"f.txt","expected_revision":page["revision"],"dry_run":true,"edits":[{"old_string":"alpha","new_string":"one"},{"old_string":"beta","new_string":"two"}]});
        let preview = tool.run(request.clone(), &cx).await.unwrap();
        assert_eq!(preview["dry_run"], true);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "alpha\nbeta\n"
        );
        request["dry_run"] = json!(false);
        let result = tool.run(request, &cx).await.unwrap();
        assert_eq!(result["edits"].as_array().unwrap().len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert_ne!(result["revision"], page["revision"]);
    }
}
