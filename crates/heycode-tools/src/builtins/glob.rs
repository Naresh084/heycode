//! `glob` — pattern-based file search over the workspace tree.

use std::sync::Arc;

use heycode_core::ToolSpec;
use serde_json::{Value, json};

use crate::builtins::{GLOB_CAP, SKIP_DIRS, arg_str, arg_str_opt, filesystem_error, resolve_path};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) fn tool(filesystem: heycode_exec::FileSystemService) -> Arc<dyn Tool> {
    Arc::new(GlobTool { filesystem })
}

struct GlobTool {
    filesystem: heycode_exec::FileSystemService,
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "glob".to_owned(),
        description: "List files matching a glob pattern (`*`, `?`, `**`; `/`-separated). \
                      Vendored directories (.git, node_modules, target, dist, .next, venv) are \
                      skipped; results are sorted and capped at 100."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob such as `src/**/*.rs` or `*.md`."},
                "path": {"type": "string", "description": "Directory to search; defaults to the working directory."}
            },
            "required": ["pattern"]
        }),
    }
}

#[async_trait::async_trait]
impl Tool for GlobTool {
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
        let pattern = arg_str(&args, "pattern")?;
        let raw_root = arg_str_opt(&args, "path").unwrap_or(".");
        let root = resolve_path(&self.filesystem, cx, raw_root)?;
        let spec = heycode_exec::GlobSpec::new(root.clone(), pattern, SKIP_DIRS, GLOB_CAP)
            .map_err(|error| filesystem_error(&error, "search", raw_root, Some(root.as_path())))?;
        let result = self
            .filesystem
            .glob(spec, cx.cancellation.clone())
            .await
            .map_err(|error| filesystem_error(&error, "search", raw_root, Some(root.as_path())))?;
        let mut out = String::new();
        for (idx, rel) in result.matches().iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(rel);
        }
        if result.total_matches() > result.matches().len() {
            out.push_str(&format!(
                "\n(showing {} of {}{} matches)",
                result.matches().len(),
                if result.report().incomplete() {
                    "at least "
                } else {
                    ""
                },
                result.total_matches()
            ));
        }
        if let Some(notice) = result.report().notice() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&notice);
        }
        Ok(Value::String(out))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Path;

    async fn run_glob(dir: &Path, args: Value) -> Result<Value, ToolError> {
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
        for f in [
            "a.rs",
            "b.txt",
            "sub/c.rs",
            "sub/deep/d.rs",
            "node_modules/pkg/x.rs",
            ".git/config",
            "target/debug/t.rs",
            "dist/bundle.js",
            ".next/cache.js",
            "venv/bin/python.rs",
        ] {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn matches_recursively_and_skips_vendored_dirs() {
        let dir = seeded_tree().await;
        let out = run_glob(dir.path(), json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert_eq!(out, json!("a.rs\nsub/c.rs\nsub/deep/d.rs"));
    }

    #[tokio::test]
    async fn question_mark_matches_single_characters() {
        let dir = seeded_tree().await;
        let out = run_glob(dir.path(), json!({"pattern": "?.txt"}))
            .await
            .unwrap();
        assert_eq!(out, json!("b.txt"));
    }

    #[tokio::test]
    async fn caps_results_with_a_footer() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..120 {
            std::fs::write(dir.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let out = run_glob(dir.path(), json!({"pattern": "*.txt"}))
            .await
            .unwrap();
        let text = out.as_str().unwrap();
        assert!(
            text.contains("(showing 100 of 120 matches)"),
            "got tail: {}",
            &text[text.len().saturating_sub(60)..]
        );
        assert_eq!(text.lines().count(), 101, "100 paths plus the footer line");
    }

    #[tokio::test]
    async fn missing_search_root_errors_with_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_glob(dir.path(), json!({"pattern": "*.rs", "path": "nowhere"}))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("File not found:"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn spec_requires_pattern() {
        let s = spec();
        assert_eq!(s.parameters["required"], json!(["pattern"]));
    }
}
