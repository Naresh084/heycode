//! Built-in tool implementations and their shared helpers.
//!
//! Shared here: argument extraction, filesystem failure projection, and the
//! explicit file-search policy passed to the replaceable filesystem service.

pub(crate) mod bash;
pub(crate) mod edit;
pub(crate) mod glob;
pub(crate) mod grep;
pub(crate) mod multi_edit;
pub(crate) mod read;
pub(crate) mod read_many;
pub mod terminal;
#[cfg(test)]
pub(crate) mod todo;
pub mod web;
pub(crate) mod write;

use serde_json::Value;

use crate::tool::{ToolCtx, ToolError};

/// Directories never descended into by `glob` and `grep`.
pub(crate) const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".next", "venv"];

/// Maximum matches returned by `glob` before a truncation footer.
pub(crate) const GLOB_CAP: usize = 100;

/// Maximum output lines returned by `grep` before a truncation footer.
pub(crate) const GREP_CAP: usize = 200;

/// Extract a required string argument.
///
/// # Errors
/// When `key` is absent or not a JSON string.
pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("\"{key}\" must be passed as a string argument")))
}

/// Extract an optional string argument.
pub(crate) fn arg_str_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Extract an optional boolean argument.
#[cfg(test)]
pub(crate) fn arg_bool_opt(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Resolve a model path through the active filesystem Provider.
///
/// # Errors
/// Invalid boundary values and provider failures become bounded model-facing
/// failures without raw operating-system diagnostics.
pub(crate) fn resolve_path(
    filesystem: &heycode_exec::FileSystemService,
    cx: &ToolCtx,
    raw: &str,
) -> Result<heycode_exec::ResolvedPath, ToolError> {
    let request = heycode_exec::PathRequest::new(&cx.cwd, raw)
        .map_err(|error| filesystem_error(&error, "resolve", raw, None))?;
    filesystem
        .resolve(request)
        .map_err(|error| filesystem_error(&error, "resolve", raw, None))
}

/// Project one fixed provider failure into recoverable tool vocabulary.
pub(crate) fn filesystem_error(
    error: &heycode_exec::FileSystemError,
    operation: &str,
    raw: &str,
    path: Option<&std::path::Path>,
) -> ToolError {
    let shown = path.map_or_else(|| raw.to_owned(), |path| path.display().to_string());
    let message = match error.code() {
        heycode_exec::FileSystemErrorCode::NotFound => format!("File not found: {shown}"),
        heycode_exec::FileSystemErrorCode::NotFile => format!("{shown} is not a file"),
        heycode_exec::FileSystemErrorCode::NotDirectory => format!("{shown} is not a directory"),
        heycode_exec::FileSystemErrorCode::PermissionDenied => {
            format!("filesystem denied {operation} for {shown}")
        }
        heycode_exec::FileSystemErrorCode::Cancelled => {
            "filesystem operation was cancelled".to_owned()
        }
        heycode_exec::FileSystemErrorCode::ServiceStopped => {
            "filesystem service is unavailable".to_owned()
        }
        heycode_exec::FileSystemErrorCode::InvalidSpec => {
            format!("invalid filesystem {operation} request for {raw}")
        }
        heycode_exec::FileSystemErrorCode::OutsideAllowedRoots => format!(
            "Access to {raw} is outside the allowed filesystem roots. Use a path under the workspace or request an approved external root."
        ),
        heycode_exec::FileSystemErrorCode::PathTraversal => format!(
            "{raw} crosses a protected path or symlink boundary. Use its canonical path inside an allowed root."
        ),
        heycode_exec::FileSystemErrorCode::ReadOnlyRoot => {
            format!("{raw} is under a read-only filesystem root. Use a writable workspace path.")
        }
        heycode_exec::FileSystemErrorCode::StaleObservation => {
            format!("{raw} changed on disk since you read it — re-read it before retrying.")
        }
        heycode_exec::FileSystemErrorCode::ChangedAtCommit => {
            format!("{raw} changed during the filesystem commit — re-read it and retry.")
        }
        heycode_exec::FileSystemErrorCode::InvalidOutput => format!(
            "failed to {operation} {shown}: filesystem provider returned invalid output (invalid_output)"
        ),
        code => format!(
            "failed to {operation} {shown}: filesystem operation failed ({})",
            code.as_str()
        ),
    };
    ToolError::new(message)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) fn test_filesystem(root: &std::path::Path) -> heycode_exec::FileSystemService {
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root,
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("test root")])
    .expect("test policy");
    heycode_exec::FileSystemService::local(policy).expect("local test filesystem")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn arg_helpers_extract_and_reject() {
        let args = serde_json::json!({"s": "v", "b": true});
        assert_eq!(arg_str(&args, "s").unwrap(), "v");
        assert_eq!(arg_str_opt(&args, "missing"), None);
        assert!(arg_bool_opt(&args, "b").unwrap());
        assert!(arg_str(&args, "n").is_err());
    }

    #[test]
    fn filesystem_policy_failures_tell_the_model_how_to_recover() {
        let outside = filesystem_error(
            &heycode_exec::FileSystemError::new(
                heycode_exec::FileSystemErrorCode::OutsideAllowedRoots,
            ),
            "read",
            "../outside.txt",
            None,
        );
        assert!(outside.message.contains("allowed filesystem roots"));
        assert!(outside.message.contains("approved external root"));

        let raced = filesystem_error(
            &heycode_exec::FileSystemError::new(heycode_exec::FileSystemErrorCode::ChangedAtCommit),
            "write",
            "file.txt",
            None,
        );
        assert!(raced.message.contains("re-read"));
        assert!(raced.message.contains("retry"));
    }
}
