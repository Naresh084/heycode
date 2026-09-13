//! Default model controls over native execution jobs.
use crate::{ExecutionJobService, JobId, MonitorConfig};
use heycode_core::ToolSpec;
use heycode_exec::{OutputStream, ShellRequest};
use heycode_session::InboxDelivery;
use heycode_tools::{Tool, ToolCallInput, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};
use std::sync::Arc;

fn string<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("{name} must be a string")))
}
fn job(args: &Value) -> Result<JobId, ToolError> {
    JobId::parse(string(args, "job_id")?).map_err(|e| ToolError::new(e.to_string()))
}
fn error(e: impl std::fmt::Display) -> ToolError {
    ToolError::new(e.to_string())
}

pub(crate) fn tools(
    execution: Arc<ExecutionJobService>,
    registry: Arc<ToolRegistry>,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(OutputTool(execution.clone())),
        Arc::new(JobControlTool(execution.clone())),
        Arc::new(MonitorTool(execution.clone(), None)),
        Arc::new(RunTool {
            execution,
            registry,
        }),
    ]
}
struct OutputTool(Arc<ExecutionJobService>);
#[async_trait::async_trait]
impl Tool for OutputTool {
    fn model_replacement(&self) -> Option<&'static str> {
        Some("job_control")
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::ReadOnly
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
        name:"job_output".to_owned(),
        description:"Read retained output while a job runs or after it settles. Non-consuming byte cursors are independent for stdout, stderr and terminal. lost_bytes reports overflow; next_offset pages forward. PTY stderr is merged into terminal.".to_owned(),
        parameters:json!({"type":"object","additionalProperties":false,"required":["job_id"],"properties":{"job_id":{"type":"string"},"stream":{"type":"string","enum":["stdout","stderr","terminal"]},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536}}}),
    }
    }
    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let execution = self.0.scoped().map_err(error)?;
        let id = job(&args)?;
        let output = execution
            .output(&id)
            .ok_or_else(|| error("no retained output for this job"))?;
        let stream = match args.get("stream").and_then(Value::as_str).unwrap_or(
            if execution.job_terminal(&id).is_some() {
                "terminal"
            } else {
                "stdout"
            },
        ) {
            "stdout" => OutputStream::Stdout,
            "stderr" => OutputStream::Stderr,
            "terminal" => OutputStream::Terminal,
            _ => return Err(error("unknown stream")),
        };
        let offset = match args.get("offset") {
            None => 0,
            Some(v) => v
                .as_u64()
                .ok_or_else(|| error("offset must be a nonnegative integer"))?,
        };
        let limit = match args.get("limit") {
            None => execution.config().inline_bytes as u64,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=65536).contains(n))
                .ok_or_else(|| error("limit must be 1..=65536"))?,
        };
        Ok(
            json!({"job_id":id.as_str(),"terminal_id":execution.job_terminal(&id).map(|v|v.to_string()),"ended":output.ended(),"interrupted":output.interrupted(),"persistence_error":output.persistence_error(),"page":output.read(stream,offset,limit as usize)}),
        )
    }
}
struct JobControlTool(Arc<ExecutionJobService>);

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum JobControlRequest {
    List,
    Output {
        job_id: String,
        stream: Option<String>,
        offset: Option<u64>,
        limit: Option<u64>,
    },
    Cancel {
        job_id: String,
    },
}

#[async_trait::async_trait]
impl Tool for JobControlTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_control".into(),
            description: "Inspect background execution jobs, retrieve retained output, or request cancellation. Agent conversations use agent_control. Foreground tools already return their output; use output only for background or clipped output. Cancellation is requested until the job actually settles. Output byte cursors are independent for stdout, stderr and terminal; next_offset pages forward and lost_bytes reports overflow.".into(),
            parameters: json!({"type":"object","additionalProperties":false,"required":["action"],
                "properties":{
                    "action":{"type":"string","enum":["list","output","cancel"]},
                    "job_id":{"type":"string","description":"Job ID from execution; never an agent conversation ID"},
                    "stream":{"type":"string","enum":["stdout","stderr","terminal"]},
                    "offset":{"type":"integer","minimum":0},
                    "limit":{"type":"integer","minimum":1,"maximum":65536}
                },
                "oneOf":[
                    {"properties":{"action":{"const":"list"}}},
                    {"properties":{"action":{"enum":["output","cancel"]}},"required":["job_id"]}
                ]
            }),
        }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let request: JobControlRequest = serde_json::from_value(args)
            .map_err(|e| error(format!("invalid job_control request: {e}")))?;
        let execution = self.0.scoped().map_err(error)?;
        match request {
            JobControlRequest::List => Ok(
                json!({"action":"list","jobs":execution.jobs().list().into_iter()
                .filter(|job| !job.coordinator).collect::<Vec<_>>()}),
            ),
            JobControlRequest::Output {
                job_id,
                stream,
                offset,
                limit,
            } => {
                let mut args = json!({"job_id":job_id});
                if let Some(stream) = stream {
                    args["stream"] = json!(stream);
                }
                if let Some(offset) = offset {
                    args["offset"] = json!(offset);
                }
                if let Some(limit) = limit {
                    args["limit"] = json!(limit);
                }
                let mut result = OutputTool(self.0.clone()).run(args, cx).await?;
                result["action"] = json!("output");
                Ok(result)
            }
            JobControlRequest::Cancel { job_id } => {
                let id = JobId::parse(&job_id).map_err(error)?;
                let requested = execution.jobs().cancel(&id);
                Ok(
                    json!({"action":"cancel","job_id":id.as_str(),"requested":requested,
                    "status":if requested {"requested"} else {"not_running"}}),
                )
            }
        }
    }
}

struct MonitorTool(Arc<ExecutionJobService>, Option<heycode_exec::ShellService>);
#[async_trait::async_trait]
impl Tool for MonitorTool {
    fn rebind_workspace(
        &self,
        _filesystem: &heycode_exec::FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self(self.0.clone(), Some(shell.clone()))))
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
        name:"monitor".to_owned(),
        description:"Watch output from one command OR an existing job. Deliver bounded matching events while it stays alive. Filters are case-sensitive substrings. Debounce, duplicate suppression, a rate limit and a lifetime event budget bound model wakes. Use ready_when/stop_when for targeted completion, job_control(action=cancel) to stop. A command and watcher receive separate correlated job ids.".to_owned(),
        parameters:json!({"type":"object","additionalProperties":false,"oneOf":[{"required":["command"]},{"required":["job_id"]}],"properties":{
            "command":{"type":"string"},"job_id":{"type":"string"},
            "watch":{"type":"object","additionalProperties":false,"properties":{
                "contains":{"type":"string","maxLength":512},"exclude":{"type":"string","maxLength":512},"stream":{"type":"string","enum":["all","stdout","stderr","terminal"]},"ready_when":{"type":"string","maxLength":512},"stop_when":{"type":"string","maxLength":512},"debounce_ms":{"type":"integer","minimum":100,"maximum":60000},"min_interval_ms":{"type":"integer","minimum":1000,"maximum":60000},"dedupe_ms":{"type":"integer","minimum":0,"maximum":60000},"max_events":{"type":"integer","minimum":1,"maximum":32},"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000},"stop_source":{"type":"boolean"}
            }}
        }}),
    }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let execution = self.0.scoped().map_err(error)?;
        let config: MonitorConfig =
            serde_json::from_value(args.get("watch").cloned().unwrap_or_else(|| json!({})))
                .map_err(error)?;
        config.validate().map_err(error)?;
        let owns = args.get("command").is_some();
        if owns == args.get("job_id").is_some() {
            return Err(error("provide exactly one of command or job_id"));
        }
        let source = if owns {
            let request = ShellRequest::new(string(&args, "command")?)
                .and_then(|r| r.with_cwd(cx.cwd.clone()))
                .and_then(|r| r.with_timeout(std::time::Duration::from_millis(config.timeout_ms)))
                .map_err(error)?;
            match &self.1 {
                Some(shell) => execution.start_shell_with_shell(
                    shell,
                    "monitored command",
                    request,
                    InboxDelivery::Inject,
                ),
                None => execution.start_shell("monitored command", request, InboxDelivery::Inject),
            }
            .map_err(error)?
        } else {
            job(&args)?
        };
        match execution.start_monitor(&source, config, owns) {
            Ok(id) => {
                Ok(json!({"job_id":id.as_str(),"source_job_id":source.as_str(),"kind":"monitor"}))
            }
            Err(e) => {
                if owns {
                    let _requested = execution.jobs().cancel(&source);
                }
                Err(error(e))
            }
        }
    }
}
struct RunTool {
    execution: Arc<ExecutionJobService>,
    registry: Arc<ToolRegistry>,
}
#[async_trait::async_trait]
impl Tool for RunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
        name:"run_tool".to_owned(),
        description:"Execute a cancellation-capable tool using normal target approval and guards. background=true returns a job id immediately. Otherwise start in the foreground; the configured timeout or UI can promote the same job without restarting it. Foreground execution returns its output directly. Use job_control(action=output) only for background or clipped output, action=list for status and action=cancel to cancel. Supported tools include bash and connected MCP tools; unsupported targets fail before dispatch.".to_owned(),
        parameters:json!({"type":"object","additionalProperties":false,"required":["tool","arguments"],"properties":{"tool":{"type":"string"},"arguments":{"type":"object"},"background":{"type":"boolean"}}}),
    }
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let execution = self.execution.scoped().map_err(error)?;
        let input = ToolCallInput {
            name: string(&args, "tool")?.to_owned(),
            args: args
                .get("arguments")
                .filter(|v| v.is_object())
                .cloned()
                .ok_or_else(|| error("arguments must be an object"))?,
        };
        let background = match args.get("background") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| error("background must be boolean"))?,
        };
        let id = execution
            .start_tool_with_mode(
                &self.registry,
                input,
                if background {
                    InboxDelivery::FollowUp
                } else {
                    InboxDelivery::Inject
                },
                !background,
            )
            .map_err(error)?;
        if background {
            return Ok(json!({"job_id":id.as_str(),"background":true}));
        }
        let promoted = execution.foreground_wait(&id);
        let _lease = crate::execution_foreground::ForegroundLease {
            service: &execution,
            id: id.clone(),
            promoted: promoted.clone(),
        };
        execution.principal.bus.emit(crate::UiEvent::Info {
            text: format!(
                "Started foreground tool {id}; task controls can move active work to the background"
            ),
        });
        let result = tokio::select! {
            result=execution.jobs().wait_for_settlement(&id)=>result.map(|outcome|json!({"job_id":id.as_str(),"outcome":outcome.name(),"output":execution.output(&id).map(|o|o.read(OutputStream::Stdout,0,execution.config().inline_bytes))})).map_err(error),
            ()=execution.automatic_promotion(&id)=>Ok(json!({"job_id":id.as_str(),"background":true,"promoted":true,"automatic":true})),
            ()=promoted.cancelled()=>Ok(json!({"job_id":id.as_str(),"background":true,"promoted":true})),
            ()=cx.cancellation.cancelled()=>{
                let _requested=execution.jobs().cancel(&id);
                match tokio::time::timeout(std::time::Duration::from_secs(5),execution.jobs().wait_for_settlement(&id)).await {
                    Ok(Ok(outcome))=>Ok(json!({"job_id":id.as_str(),"outcome":outcome.name()})),
                    _=>Err(error(format!("cancellation requested for {id}; settlement unconfirmed"))),
                }
            }
        };
        execution.end_foreground_wait(&id);
        result
    }
}
