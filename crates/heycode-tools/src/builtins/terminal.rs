//! E07 — model-facing persistent terminals.
//!
//! A terminal outlives the tool call that made it, which is exactly why the
//! model never holds a handle: every tool takes an opaque id and the registry
//! owns the process. The **owner** is the session id, supplied by the host and
//! never by the model, so a model cannot name another session's terminal — a
//! foreign id is indistinguishable from an unknown one.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::ToolSpec;
use heycode_exec::{
    MAX_TERMINAL_READ_BYTES, ProcessError, ProcessErrorCode, ProcessSpec, TerminalId,
    TerminalOwner, TerminalService, TerminalSize, TerminalSpec,
};
use serde_json::{Value, json};

use crate::{Tool, ToolCtx, ToolError};

/// Bound on text the model may write into a terminal in one call.
const MAX_TERMINAL_WRITE_BYTES: usize = 64 * 1024;

/// Shared binding every terminal tool holds.
struct TerminalTools {
    terminals: TerminalService,
    /// Resolving the executable is the subprocess Provider's job; the terminal
    /// registry deliberately owns lifetime, not PATH policy.
    subprocess: Option<heycode_exec::SubprocessService>,
    shell: Option<heycode_exec::ShellService>,
    owner: TerminalOwner,
    cwd: std::path::PathBuf,
}

impl TerminalTools {
    fn owner(&self) -> TerminalOwner {
        heycode_exec::current_terminal_owner().unwrap_or_else(|| self.owner.clone())
    }
}

/// Map a process failure to model-facing text.
///
/// Every arm is fixed text: a terminal's own output is returned through `read`,
/// never smuggled into a diagnostic.
fn tool_error(error: &ProcessError) -> ToolError {
    ToolError::new(match error.code() {
        ProcessErrorCode::UnknownTerminal => {
            "no such terminal for this session — call terminal_list"
        }
        ProcessErrorCode::TerminalExited => "the terminal's process has exited",
        ProcessErrorCode::TerminalCapacity => {
            "too many open terminals — close one with terminal_kill"
        }
        ProcessErrorCode::InvalidSpec => "the terminal request is invalid",
        ProcessErrorCode::Cancelled => "the terminal operation was cancelled",
        _ => "the terminal operation failed",
    })
}

fn terminal_id(args: &Value) -> Result<TerminalId, ToolError> {
    let raw = args
        .get("terminal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("`terminal_id` must be a string"))?;
    TerminalId::parse(raw).map_err(|error| tool_error(&error))
}

fn size(args: &Value) -> Result<Option<TerminalSize>, ToolError> {
    let (Some(cols), Some(rows)) = (
        args.get("cols").and_then(Value::as_u64),
        args.get("rows").and_then(Value::as_u64),
    ) else {
        return Ok(None);
    };
    let convert = |value: u64| u16::try_from(value).ok().filter(|value| *value > 0);
    let (Some(cols), Some(rows)) = (convert(cols), convert(rows)) else {
        return Err(ToolError::new("`cols` and `rows` must be 1..=65535"));
    };
    TerminalSize::new(cols, rows)
        .map(Some)
        .map_err(|error| tool_error(&error))
}

/// `terminal_open {command, args?, cols?, rows?}` — start a persistent shell.
pub struct TerminalOpenTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalOpenTool {
    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        let subprocess = shell.subprocess();
        let cwd = self.0.cwd.clone();
        Some(Arc::new(Self(Arc::new(TerminalTools {
            terminals: self.0.terminals.clone(),
            subprocess,
            shell: Some(shell.clone()),
            owner: self.0.owner.clone(),
            cwd,
        }))))
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_open".to_owned(),
            description: "Start a persistent terminal that survives across tool calls. \
                          Returns its id. Use it for interactive or long-running commands; \
                          use bash for one-shot commands."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["command"],
                "properties": {
                    "command": {"type": "string", "description": "Executable to run, resolved on PATH"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "cols": {"type": "integer", "description": "Terminal width; defaults to 80"},
                    "rows": {"type": "integer", "description": "Terminal height; defaults to 24"}
                }
            }),
        }
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("`command` must be a string"))?;
        let subprocess = match &self.0.shell {
            Some(shell) => shell.subprocess(),
            None => self.0.subprocess.clone(),
        };
        let subprocess = subprocess
            .as_ref()
            .ok_or_else(|| ToolError::new("workspace shell does not support terminal launch"))?;
        let program = subprocess
            .resolve_program(std::ffi::OsStr::new(command))
            .map_err(|error| tool_error(&error))?;
        let argv: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let cwd = if self.0.shell.is_some() || heycode_exec::current_terminal_owner().is_some() {
            &cx.cwd
        } else {
            &self.0.cwd
        };
        let process = ProcessSpec::new(program, cwd)
            .and_then(|spec| spec.with_args(argv))
            .and_then(|spec| spec.with_environment(heycode_exec::safe_environment_snapshot()))
            .map(ProcessSpec::with_interactive_stdio)
            .map_err(|error| tool_error(&error))?;
        let mut spec = TerminalSpec::new(process).map_err(|error| tool_error(&error))?;
        if let Some(size) = size(&args)? {
            spec = spec.with_size(size);
        }
        let id = self
            .0
            .terminals
            .open_with_subprocess(&self.0.owner(), spec, cx.cancellation.clone(), subprocess)
            .await
            .map_err(|error| tool_error(&error))?;
        Ok(json!({"terminal_id": id.as_str()}))
    }
}

/// `terminal_write {terminal_id, input}` — send keystrokes.
pub struct TerminalWriteTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_write".to_owned(),
            description: "Send input to a persistent terminal. Include a trailing newline to \
                          submit a command. Read the output separately with terminal_read."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["terminal_id", "input"],
                "properties": {
                    "terminal_id": {"type": "string"},
                    "input": {"type": "string", "description": "Exact bytes to send, including any newline"}
                }
            }),
        }
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let id = terminal_id(&args)?;
        let input = args
            .get("input")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("`input` must be a string"))?;
        if input.len() > MAX_TERMINAL_WRITE_BYTES {
            return Err(ToolError::new("`input` exceeds the 64 KiB write bound"));
        }
        self.0
            .terminals
            .write(&self.0.owner(), &id, input.as_bytes())
            .await
            .map_err(|error| tool_error(&error))?;
        Ok(json!({"written_bytes": input.len()}))
    }
}

/// `terminal_read {terminal_id, max_bytes?}` — drain buffered output.
pub struct TerminalReadTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_read".to_owned(),
            description: "Read and drain buffered output from a persistent terminal. \
                          Output already read is not returned again. When `dropped_bytes` is \
                          non-zero, older output was discarded to stay within the buffer."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["terminal_id"],
                "properties": {
                    "terminal_id": {"type": "string"},
                    "max_bytes": {"type": "integer", "description": "At most 65536; defaults to that"}
                }
            }),
        }
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let id = terminal_id(&args)?;
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(MAX_TERMINAL_READ_BYTES);
        let read = self
            .0
            .terminals
            .read(&self.0.owner(), &id, max_bytes)
            .await
            .map_err(|error| tool_error(&error))?;
        let ended = read.ended();
        let dropped = read.dropped_bytes();
        // Terminal output is arbitrary bytes; anything that is not valid UTF-8
        // is replaced rather than refused, so a binary burst cannot make a
        // terminal permanently unreadable.
        let output = String::from_utf8_lossy(read.bytes()).into_owned();
        Ok(json!({
            "output": output,
            "dropped_bytes": dropped,
            "ended": ended,
        }))
    }
}

/// `terminal_resize {terminal_id, cols, rows}` — change the live geometry.
pub struct TerminalResizeTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalResizeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_resize".to_owned(),
            description: "Resize a persistent terminal. Programs that redraw on window change \
                          observe the new size immediately."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["terminal_id", "cols", "rows"],
                "properties": {
                    "terminal_id": {"type": "string"},
                    "cols": {"type": "integer"},
                    "rows": {"type": "integer"}
                }
            }),
        }
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let id = terminal_id(&args)?;
        let size = size(&args)?.ok_or_else(|| ToolError::new("`cols` and `rows` are required"))?;
        self.0
            .terminals
            .resize(&self.0.owner(), &id, size)
            .await
            .map_err(|error| tool_error(&error))?;
        Ok(json!({"cols": size.cols(), "rows": size.rows()}))
    }
}

/// `terminal_kill {terminal_id}` — end a terminal and reap its process tree.
pub struct TerminalKillTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalKillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_kill".to_owned(),
            description: "End a persistent terminal and every process it started. \
                          This is a hard kill, not a graceful shutdown."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["terminal_id"],
                "properties": {"terminal_id": {"type": "string"}}
            }),
        }
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let id = terminal_id(&args)?;
        let exit = self
            .0
            .terminals
            .kill(&self.0.owner(), &id)
            .await
            .map_err(|error| tool_error(&error))?;
        Ok(json!({"killed": true, "success": exit.is_success()}))
    }
}

/// `terminal_list {}` — this session's live terminals.
pub struct TerminalListTool(Arc<TerminalTools>);

#[async_trait]
impl Tool for TerminalListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_list".to_owned(),
            description: "List this session's persistent terminals with their size and \
                          buffered output state."
                .to_owned(),
            parameters: json!({"type": "object", "additionalProperties": false}),
        }
    }

    async fn run(&self, _args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let rows: Vec<Value> = self
            .0
            .terminals
            .list(&self.0.owner())
            .await
            .into_iter()
            .map(|status| {
                json!({
                    "terminal_id": status.id().as_str(),
                    "cols": status.size().cols(),
                    "rows": status.size().rows(),
                    "pending_bytes": status.pending_bytes(),
                    "dropped_bytes": status.dropped_bytes(),
                    "ended": status.output_ended(),
                })
            })
            .collect();
        Ok(Value::Array(rows))
    }
}

/// Build every terminal tool bound to one owner.
///
/// `owner` must be the host's session identity, never model-supplied input:
/// scoping is only real if the model cannot choose whose terminals it names.
#[must_use]
pub fn terminal_tools(
    terminals: TerminalService,
    subprocess: heycode_exec::SubprocessService,
    owner: TerminalOwner,
    cwd: std::path::PathBuf,
) -> Vec<Arc<dyn Tool>> {
    let shared = Arc::new(TerminalTools {
        shell: None,
        terminals,
        subprocess: Some(subprocess),
        owner,
        cwd,
    });
    vec![
        Arc::new(TerminalOpenTool(shared.clone())),
        Arc::new(TerminalWriteTool(shared.clone())),
        Arc::new(TerminalReadTool(shared.clone())),
        Arc::new(TerminalResizeTool(shared.clone())),
        Arc::new(TerminalKillTool(shared.clone())),
        Arc::new(TerminalListTool(shared)),
    ]
}
