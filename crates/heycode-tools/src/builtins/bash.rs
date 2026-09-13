//! `bash` compatibility tool over the plugin-owned shell service.

use std::sync::Arc;
use std::time::Duration;

use heycode_core::ToolSpec;
use heycode_exec::{ProcessExit, ShellRequest, ShellService};
use serde_json::{Value, json};

use crate::builtins::arg_str;
use crate::tool::{Tool, ToolCtx, ToolError};

/// Combined stdout+stderr tail kept after capping.
const OUTPUT_TAIL_CAP: usize = 64 * 1024;

pub(crate) fn tool(shell: ShellService) -> Arc<dyn Tool> {
    Arc::new(BashTool { shell })
}

struct BashTool {
    shell: ShellService,
}

fn spec() -> ToolSpec {
    ToolSpec {
        name: "bash".to_owned(),
        description:
            "Run a command through the configured local shell from the working directory. \
                      The resolved child environment excludes credential-shaped names. Output is \
                      capped to a trailing 64 KiB; a nonzero exit code or timeout is reported in \
                      the output, not as an error."
                .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command line to execute."},
                "timeout_ms": {"type": "integer", "minimum": 1, "description": "Optional wall-clock budget in milliseconds; otherwise the shell provider default is used."}
            },
            "required": ["command"]
        }),
    }
}

/// Keep only the trailing `cap` bytes of `body`, never splitting a UTF-8 char.
fn tail_string(body: String, cap: usize) -> String {
    if body.len() <= cap {
        return body;
    }
    let bytes = body.as_bytes();
    let mut start = body.len() - cap;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    body[start..].to_owned()
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus { configured: Some(true), detail: "Shell service is bound; executable availability, sandbox and call permissions are checked when running.".into() }
    }

    fn supports_background(&self) -> bool {
        true
    }

    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(tool(shell.clone()))
    }

    fn spec(&self) -> ToolSpec {
        spec()
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        self.execute(args, cx, None).await
    }
    async fn run_output_observed(
        &self,
        args: Value,
        cx: &ToolCtx,
        sink: Arc<dyn heycode_exec::ProcessOutputSink>,
    ) -> Result<crate::ToolOutput, ToolError> {
        self.execute(args, cx, Some(sink))
            .await
            .map(crate::ToolOutput::plain)
    }
}
impl BashTool {
    async fn execute(
        &self,
        args: Value,
        cx: &ToolCtx,
        sink: Option<Arc<dyn heycode_exec::ProcessOutputSink>>,
    ) -> Result<Value, ToolError> {
        let command_line = arg_str(&args, "command")?;
        let timeout = match args.get("timeout_ms") {
            None => None,
            Some(value) => {
                let millis = value.as_u64().ok_or_else(|| {
                    ToolError::new("timeout_ms must be a positive integer no greater than 86400000")
                })?;
                Some(Duration::from_millis(millis))
            }
        };
        let mut request = ShellRequest::new(command_line)
            .and_then(|request| request.with_cwd(cx.cwd.clone()))
            .map_err(|_| ToolError::new("invalid shell command or working directory"))?;
        if let Some(timeout) = timeout {
            request = request.with_timeout(timeout).map_err(|_| {
                ToolError::new("timeout_ms must be a positive integer no greater than 86400000")
            })?;
        }
        let resolved = self
            .shell
            .resolve(request)
            .map_err(|_| ToolError::new("shell request could not be resolved"))?;
        let timeout_ms = resolved.timeout().map_or(0, |timeout| timeout.as_millis());

        let output = match sink {
            Some(sink) => {
                self.shell
                    .execute_streaming(resolved, cx.cancellation.clone(), sink)
                    .await
            }
            None => self.shell.execute(resolved, cx.cancellation.clone()).await,
        }
        .map_err(|error| match error.code() {
            heycode_exec::ProcessErrorCode::Cancelled => {
                ToolError::new("shell command was cancelled and its process tree settled")
            }
            heycode_exec::ProcessErrorCode::OutputLimit => {
                ToolError::new("shell output exceeded the configured capture limit")
            }
            _ => ToolError::new("shell process infrastructure failed"),
        })?;

        let mut body = String::from_utf8_lossy(output.stdout()).into_owned();
        if !output.stderr().is_empty() {
            body.push_str("\n[stderr]\n");
            body.push_str(output.stderr());
        }
        let truncated = output.truncated() || body.len() > OUTPUT_TAIL_CAP;
        body = tail_string(body, OUTPUT_TAIL_CAP);
        if body.ends_with('\n') {
            body.pop();
        }
        let tail_line = match output.exit() {
            ProcessExit::Exited { code } => format!("[exit code: {code}]"),
            ProcessExit::Signalled { signal } => format!(
                "[killed by signal: {}]",
                signal.map_or_else(|| "?".to_owned(), |signal| signal.to_string())
            ),
            ProcessExit::TimedOut | ProcessExit::InactivityTimedOut => {
                format!("[timed out after {timeout_ms}ms]")
            }
            _ => "[terminated abnormally]".to_owned(),
        };
        if truncated {
            body.push_str("\n[output truncated to trailing 65536 bytes]");
        }
        body.push('\n');
        body.push_str(&tail_line);
        Ok(Value::String(body))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    async fn run_bash(dir: &Path, args: Value) -> Result<Value, ToolError> {
        let config = heycode_exec::LocalShellConfig::platform(
            dir.canonicalize().unwrap(),
            Duration::from_secs(30),
        )
        .unwrap();
        tool(ShellService::local(config))
            .run(
                args,
                &ToolCtx {
                    cwd: dir.to_path_buf(),
                    ..Default::default()
                },
            )
            .await
    }

    fn bash_available(dir: &Path) -> bool {
        heycode_exec::LocalShellConfig::platform(
            dir.canonicalize().unwrap(),
            Duration::from_secs(30),
        )
        .is_ok()
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let out = run_bash(dir.path(), json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert_eq!(out, json!("hello\n[exit code: 0]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_is_reported_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let result = run_bash(dir.path(), json!({"command": "echo oops >&2; exit 3"})).await;
        let out = result.unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("[stderr]\noops"), "got: {text}");
        assert!(text.ends_with("[exit code: 3]"), "got: {text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_tree_and_reports_itself() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let out = run_bash(
            dir.path(),
            json!({"command": "sleep 30", "timeout_ms": 150}),
        )
        .await
        .unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("[timed out after 150ms]"), "got: {text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_inside_the_context_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let out = run_bash(dir.path(), json!({"command": "pwd"}))
            .await
            .unwrap();
        let text = out.as_str().unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            text.starts_with(expected.to_str().unwrap()),
            "got: {text}, expected prefix: {}",
            expected.display()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolved_environment_never_contains_credential_names() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let out = run_bash(dir.path(), json!({"command": "printenv"}))
            .await
            .unwrap();
        let leaked: Vec<&str> = out
            .as_str()
            .unwrap()
            .lines()
            .filter(|line| {
                line.split('=').next().is_some_and(|name| {
                    let upper = name.to_ascii_uppercase();
                    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
                        .iter()
                        .any(|needle| upper.contains(needle))
                })
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "credential-shaped env vars reached the child process: {leaked:?}"
        );
    }

    #[tokio::test]
    async fn invalid_timeout_is_rejected_instead_of_defaulted() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        for value in [json!(0), json!(-1), json!("100")] {
            let error = run_bash(
                dir.path(),
                json!({"command": "echo no", "timeout_ms": value}),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("timeout_ms"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_output_returns_a_bounded_visible_tail() {
        let dir = tempfile::tempdir().unwrap();
        if !bash_available(dir.path()) {
            return;
        }
        let out = run_bash(
            dir.path(),
            json!({
                "command": "i=0; while [ $i -lt 7000 ]; do printf 0123456789; i=$((i+1)); done; printf tail-marker"
            }),
        )
        .await
        .unwrap();
        let text = out.as_str().unwrap();
        assert!(text.contains("tail-marker"), "{text}");
        assert!(
            text.contains("[output truncated to trailing 65536 bytes]"),
            "{text}"
        );
        assert!(
            text.len() < 66 * 1024,
            "bounded result was {} bytes",
            text.len()
        );
    }

    #[test]
    fn tail_string_keeps_the_end_on_char_boundaries() {
        assert_eq!(tail_string("abcdef".to_owned(), 4), "cdef");
        assert_eq!(tail_string("short".to_owned(), 64), "short");
        let wide = "éééé".to_owned();
        assert_eq!(tail_string(wide.clone(), 5), "éé");
        assert_eq!(tail_string(wide, 6), "ééé");
    }
}
