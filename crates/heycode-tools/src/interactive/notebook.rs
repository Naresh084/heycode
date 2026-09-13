//! Cell-aware nbformat 4 edits using the filesystem's atomic exact replacement.
use std::sync::Arc;

use heycode_core::ToolSpec;
use heycode_exec::{EditFileSpec, FileSystemService, ReadFileSpec};
use serde_json::{Value, json};

use super::{digest, file_error};
use crate::builtins::{arg_str, resolve_path};
use crate::{Tool, ToolCtx, ToolError};

const MAX_NOTEBOOK: usize = 8 * 1024 * 1024;

pub(super) fn tools(filesystem: Arc<FileSystemService>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Notebook {
            filesystem: filesystem.clone(),
            edit: false,
        }),
        Arc::new(Notebook {
            filesystem,
            edit: true,
        }),
    ]
}

struct Notebook {
    filesystem: Arc<FileSystemService>,
    edit: bool,
}

#[async_trait::async_trait]
impl Tool for Notebook {
    fn rebind_workspace(
        &self,
        filesystem: &FileSystemService,
        _shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            filesystem: Arc::new(filesystem.clone()),
            edit: self.edit,
        }))
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: if self.edit { "notebook_edit" } else { "notebook_read" }.into(),
            description: if self.edit {
                "Edit an nbformat 4 notebook cell with the revision from notebook_read. Actions: replace source, insert before cell_index (length appends), delete. Other cells and notebook metadata are preserved; edited code outputs/execution_count are cleared. Does not execute code."
            } else {
                "Read notebook cell ids/types/source and revision. Returns at most 32 cells and 32 KiB source; use start_cell to page. Revision is required by notebook_edit. Outputs are omitted."
            }.into(),
            parameters: if self.edit { json!({"type":"object","properties":{
                "path":{"type":"string"}, "expected_revision":{"type":"string"},
                "action":{"enum":["replace","insert","delete"]},
                "cell_index":{"type":"integer","minimum":0},
                "cell_id":{"type":"string","description":"Optional expected id at cell_index; mismatch rejects the edit."},
                "source":{"type":"string","maxLength":262144},
                "cell_type":{"enum":["code","markdown","raw"],"description":"Required only when inserting."}
            },"required":["path","expected_revision","action","cell_index"],"additionalProperties":false}) }
            else { json!({"type":"object","properties":{"path":{"type":"string"},"start_cell":{"type":"integer","minimum":0}},"required":["path"],"additionalProperties":false}) },
        }
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let path = resolve_path(&self.filesystem, cx, arg_str(&args, "path")?)?;
        let read = self
            .filesystem
            .read(
                ReadFileSpec::new(path.clone(), MAX_NOTEBOOK).map_err(file_error)?,
                cx.cancellation.clone(),
            )
            .await
            .map_err(file_error)?;
        if read.truncated() {
            return Err(ToolError::new(
                "Notebook exceeds 8 MiB; reduce retained outputs first.",
            ));
        }
        let revision = digest(read.bytes());
        let mut notebook: Value = serde_json::from_slice(read.bytes())
            .map_err(|_| ToolError::new("Notebook must be valid JSON."))?;
        validate(&notebook)?;
        if !self.edit {
            return projection(
                &notebook,
                &revision,
                args.get("start_cell").and_then(Value::as_u64).unwrap_or(0) as usize,
            );
        }
        if arg_str(&args, "expected_revision")? != revision {
            return Err(ToolError::new(
                "Notebook changed; notebook_read again before editing.",
            ));
        }
        mutate(&mut notebook, &args, &revision)?;
        validate(&notebook)?;
        let replacement = serde_json::to_string_pretty(&notebook)
            .map_err(|_| ToolError::new("Notebook serialization failed."))?
            + "\n";
        if replacement.len() > MAX_NOTEBOOK {
            return Err(ToolError::new(
                "Edited notebook exceeds 8 MiB; reduce retained outputs first.",
            ));
        }
        let original = std::str::from_utf8(read.bytes())
            .map_err(|_| ToolError::new("Notebook must be UTF-8."))?;
        self.filesystem
            .edit(
                EditFileSpec::new(path, original, &replacement, false).map_err(file_error)?,
                cx.cancellation.clone(),
            )
            .await
            .map_err(file_error)?;
        let mut result = edited_cell_metadata(&notebook, &args);
        result.insert("revision".into(), json!(digest(replacement.as_bytes())));
        result.insert(
            "cells".into(),
            json!(notebook["cells"].as_array().map(Vec::len)),
        );
        result.insert("executed".into(), json!(false));
        Ok(Value::Object(result))
    }
}

/// Optional presentation metadata from the successfully stored cell. Deletion
/// has no remaining edited cell; never attribute the next cell's type to it.
fn edited_cell_metadata(notebook: &Value, args: &Value) -> serde_json::Map<String, Value> {
    let mut metadata = serde_json::Map::new();
    if args["action"] == "delete" {
        return metadata;
    }
    let Some(cell) = args["cell_index"]
        .as_u64()
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| notebook["cells"].get(index))
    else {
        return metadata;
    };
    metadata.insert("cell_type".into(), cell["cell_type"].clone());
    if cell["cell_type"] == "code" {
        // language_info is the notebook's explicit code-language declaration;
        // kernelspec.language is a fallback, never kernelspec.name or .ipynb.
        let language = [
            &notebook["metadata"]["language_info"]["name"],
            &notebook["metadata"]["kernelspec"]["language"],
        ]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|language| {
            !language.trim().is_empty()
                && language.len() <= 128
                && !language.chars().any(char::is_control)
        });
        if let Some(language) = language {
            metadata.insert("language".into(), json!(language));
        }
    }
    metadata
}

fn validate(notebook: &Value) -> Result<(), ToolError> {
    let invalid = || {
        ToolError::new("Expected nbformat 4 with object metadata, valid cells and unique cell ids.")
    };
    if notebook["nbformat"] != 4
        || !notebook["nbformat_minor"].is_u64()
        || !notebook["metadata"].is_object()
    {
        return Err(invalid());
    }
    let cells = notebook["cells"].as_array().ok_or_else(invalid)?;
    if cells.len() > 10000 {
        return Err(invalid());
    }
    let mut ids = std::collections::BTreeSet::new();
    for cell in cells {
        if !cell["metadata"].is_object()
            || !matches!(
                cell["cell_type"].as_str(),
                Some("code" | "markdown" | "raw")
            )
        {
            return Err(invalid());
        }
        source(cell)?;
        if notebook["nbformat_minor"].as_u64().unwrap_or(0) >= 5 && cell.get("id").is_none() {
            return Err(invalid());
        }
        if let Some(id) = cell.get("id") {
            let id = id.as_str().ok_or_else(invalid)?;
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || !ids.insert(id)
            {
                return Err(invalid());
            }
        }
        if cell["cell_type"] == "code"
            && (!cell["outputs"].is_array()
                || !(cell["execution_count"].is_null() || cell["execution_count"].is_u64()))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn source(cell: &Value) -> Result<String, ToolError> {
    if let Some(text) = cell["source"].as_str() {
        return Ok(text.into());
    }
    let lines = cell["source"]
        .as_array()
        .ok_or_else(|| ToolError::new("Cell source must be text or an array of strings."))?;
    lines
        .iter()
        .map(|line| {
            line.as_str()
                .ok_or_else(|| ToolError::new("Cell source lines must be strings."))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.concat())
}

fn projection(notebook: &Value, revision: &str, start: usize) -> Result<Value, ToolError> {
    let cells = notebook["cells"]
        .as_array()
        .ok_or_else(|| ToolError::new("Missing cells."))?;
    if start > cells.len() {
        return Err(ToolError::new("start_cell exceeds notebook length."));
    }
    let mut rows = Vec::new();
    for (index, cell) in cells.iter().enumerate().skip(start).take(32) {
        let text = source(cell)?;
        let preview = super::prefix(&text, 1024);
        rows.push(json!({"index":index,"id":cell.get("id"),"cell_type":cell["cell_type"],"source":preview,"source_truncated":preview.len()<text.len()}));
    }
    Ok(
        json!({"revision":revision,"total_cells":cells.len(),"next_cell":if start+rows.len()<cells.len(){Some(start+rows.len())}else{None},"cells":rows}),
    )
}

fn mutate(notebook: &mut Value, args: &Value, revision: &str) -> Result<(), ToolError> {
    let index = args["cell_index"]
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| ToolError::new("cell_index must be a nonnegative integer."))?;
    let cells = notebook["cells"]
        .as_array_mut()
        .ok_or_else(|| ToolError::new("Missing cells."))?;
    let action = arg_str(args, "action")?;
    if index > cells.len() || (action != "insert" && index == cells.len()) {
        return Err(ToolError::new("cell_index is outside the notebook."));
    }
    if let Some(id) = args.get("cell_id")
        && cells.get(index).and_then(|c| c.get("id")) != Some(id)
    {
        return Err(ToolError::new("Cell id conflict; notebook_read again."));
    }
    match action {
        "delete" => {
            cells.remove(index);
        }
        "replace" | "insert" => {
            let text = arg_str(args, "source")?;
            if text.len() > 262144 {
                return Err(ToolError::new("Cell source exceeds 256 KiB."));
            }
            if action == "insert" {
                let kind = arg_str(args, "cell_type")?;
                if !matches!(kind, "code" | "markdown" | "raw") {
                    return Err(ToolError::new("Unsupported cell_type."));
                }
                let id = format!("heycode-{}-{index}", &revision[..16]);
                cells.insert(
                    index,
                    json!({"id":id,"cell_type":kind,"metadata":{},"source":""}),
                );
            }
            let cell = &mut cells[index];
            // Preserve the source representation, including multi-line list convention.
            cell["source"] = if cell["source"].is_array() {
                json!(text.split_inclusive('\n').collect::<Vec<_>>())
            } else {
                json!(text)
            };
            if cell["cell_type"] == "code" {
                cell["outputs"] = json!([]);
                cell["execution_count"] = Value::Null;
            }
        }
        _ => return Err(ToolError::new("action must be replace, insert or delete.")),
    }
    Ok(())
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn notebook_edit_language_comes_only_from_stored_code_metadata() {
        let mut notebook = json!({"metadata":{"language_info":{"name":"python"},"kernelspec":{"language":"julia","name":"python3"}},"cells":[{"cell_type":"code"},{"cell_type":"markdown"}]});
        let args =
            json!({"action":"replace","cell_index":0,"cell_type":"markdown","language":"invented"});
        let metadata = edited_cell_metadata(&notebook, &args);
        assert_eq!(metadata["cell_type"], "code");
        assert_eq!(metadata["language"], "python");
        notebook["metadata"]["language_info"] = Value::Null;
        assert_eq!(edited_cell_metadata(&notebook, &args)["language"], "julia");
        notebook["metadata"]["kernelspec"]["language"] = Value::Null;
        assert!(!edited_cell_metadata(&notebook, &args).contains_key("language"));
        notebook["metadata"]["language_info"] = json!({"name":"python\u{1b}[31m"});
        assert!(!edited_cell_metadata(&notebook, &args).contains_key("language"));
        assert!(
            !edited_cell_metadata(&notebook, &json!({"action":"insert","cell_index":1}))
                .contains_key("language")
        );
        assert!(
            edited_cell_metadata(&notebook, &json!({"action":"delete","cell_index":0})).is_empty()
        );
    }
}
