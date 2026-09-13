//! `todo_write` — replace the session task list with a validated canonical one.

use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Map, Value, json};

use crate::tool::{Tool, ToolCtx, ToolError};

/// The closed set of allowed statuses.
const STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

pub(crate) fn tool() -> Arc<dyn Tool> {
    Arc::new(TodoTool)
}

struct TodoTool;

fn spec() -> ToolSpec {
    ToolSpec {
        name: "todo_write".to_owned(),
        description: "Replace the working task list. Contents must be non-empty and unique, \
                      status one of pending|in_progress|completed, and at most one item may be \
                      in_progress."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete replacement list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string", "description": "What needs doing."},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }),
    }
}

fn validate_item(args: &Value, idx: usize) -> Result<(&str, &str), ToolError> {
    let Some(obj) = args.as_object() else {
        return Err(ToolError::new(format!(
            "todos[{idx}] must be an object with \"content\" and \"status\""
        )));
    };
    let content = obj
        .get("content")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| {
            ToolError::new(format!("todos[{idx}].content must be a non-empty string"))
        })?;
    let status = obj.get("status").and_then(Value::as_str).ok_or_else(|| {
        ToolError::new(format!(
            "todos[{idx}].status must be a string from {}",
            STATUSES.to_vec().join("|")
        ))
    })?;
    if !STATUSES.contains(&status) {
        return Err(ToolError::new(format!(
            "todos[{idx}].status {status:?} is not one of pending|in_progress|completed"
        )));
    }
    Ok((content, status))
}

#[async_trait::async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        spec()
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let Some(todos) = args.get("todos").and_then(Value::as_array) else {
            return Err(ToolError::new(
                "\"todos\" must be passed as an array argument",
            ));
        };
        let mut seen = std::collections::HashSet::with_capacity(todos.len());
        let mut in_progress = 0usize;
        let mut canonical: Vec<(String, String)> = Vec::with_capacity(todos.len());
        for (idx, item) in todos.iter().enumerate() {
            let (content, status) = validate_item(item, idx)?;
            if !seen.insert(content.to_owned()) {
                return Err(ToolError::new(format!(
                    "duplicate todo content: {content:?}"
                )));
            }
            if status == "in_progress" {
                in_progress += 1;
                if in_progress > 1 {
                    return Err(ToolError::new(
                        "at most one todo may be in_progress at a time",
                    ));
                }
            }
            canonical.push((content.to_owned(), status.to_owned()));
        }

        // Canonical echo: rebuilt objects keep field order stable for the log.
        let items: Vec<Value> = canonical
            .into_iter()
            .map(|(content, status)| {
                let mut obj = Map::with_capacity(2);
                obj.insert("content".to_owned(), Value::String(content));
                obj.insert("status".to_owned(), Value::String(status));
                Value::Object(obj)
            })
            .collect();
        Ok(Value::Array(items))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    async fn run_todo(args: Value) -> Result<Value, ToolError> {
        tool()
            .run(
                args,
                &ToolCtx {
                    cwd: std::env::temp_dir(),
                    ..Default::default()
                },
            )
            .await
    }

    #[tokio::test]
    async fn echoes_a_canonical_array_on_valid_input() {
        let out = run_todo(json!({"todos": [
            {"content": "first", "status": "completed"},
            {"content": "second", "status": "in_progress"},
            {"content": "third", "status": "pending"}
        ]}))
        .await
        .unwrap();
        assert_eq!(
            out,
            json!([
                {"content": "first", "status": "completed"},
                {"content": "second", "status": "in_progress"},
                {"content": "third", "status": "pending"}
            ])
        );
    }

    #[tokio::test]
    async fn accepts_an_empty_list_to_clear_the_board() {
        let out = run_todo(json!({"todos": []})).await.unwrap();
        assert_eq!(out, json!([]));
    }

    #[tokio::test]
    async fn rejects_duplicate_contents() {
        let err = run_todo(json!({"todos": [
            {"content": "same", "status": "pending"},
            {"content": "same", "status": "pending"}
        ]}))
        .await
        .unwrap_err();
        assert!(err.message.contains("duplicate"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn rejects_more_than_one_in_progress() {
        let err = run_todo(json!({"todos": [
            {"content": "a", "status": "in_progress"},
            {"content": "b", "status": "in_progress"}
        ]}))
        .await
        .unwrap_err();
        assert!(err.message.contains("at most one"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn rejects_unknown_status() {
        let err = run_todo(json!({"todos": [{"content": "a", "status": "done"}]}))
            .await
            .unwrap_err();
        assert!(
            err.message
                .contains("not one of pending|in_progress|completed"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn rejects_blank_content() {
        for blank in ["", "   "] {
            let err = run_todo(json!({"todos": [{"content": blank, "status": "pending"}]}))
                .await
                .unwrap_err();
            assert!(err.message.contains("non-empty"), "got: {}", err.message);
        }
    }

    #[tokio::test]
    async fn rejects_missing_or_mistyped_todos_argument() {
        assert!(run_todo(json!({})).await.is_err());
        assert!(run_todo(json!({"todos": "nope"})).await.is_err());
        assert!(
            run_todo(json!({"todos": [42]}))
                .await
                .unwrap_err()
                .message
                .contains("must be an object")
        );
    }

    #[test]
    fn spec_declares_the_closed_status_set() {
        let s = spec();
        assert_eq!(s.name, "todo_write");
        assert_eq!(
            s.parameters["properties"]["todos"]["items"]["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
    }
}
